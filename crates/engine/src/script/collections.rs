//! Live collections are rooted queries over the current DOM, not cloned JS
//! arrays. The VM's indexed host adapter exposes one own property per match.
use super::*;
use crate::dom::{Namespace, QuirksMode};
use rust_browser_javascript::HostIndexedProperties;
use std::{cell::Cell, collections::VecDeque, rc::Weak};

const CACHE_ENTRIES: usize = 24;
const CACHE_NODES: usize = 128 * 1024;
// Owner-backed children/childNodes have a fixed-size filter and one traced
// slot per accessed property. Arbitrary caller-supplied queries keep their
// own lower byte ceiling and cannot consume this allowance.
const MAX_CHILD_VIEW_BYTES: usize = 64 * 1024 * 1024;
const CHILDREN_SLOT: u32 = 0x434f_4c00;
const CHILD_NODES_SLOT: u32 = 0x434f_4c01;
const MAX_COLLECTION_QUERY_BYTES: usize = 16 * 1024;
const MAX_REFLECTION_SCAN_WORK: usize = 128 * 1024 * 1024;
const MAX_LIVE_QUERY_BYTES: usize = 2 * 1024 * 1024;
const MAX_STATIC_LIST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Hash, Eq, PartialEq)]
enum Filter {
    Children,
    ChildNodes,
    Tag {
        original: String,
        lower: String,
    },
    Namespace {
        namespace: Option<String>,
        local: String,
    },
    Classes(Vec<String>),
    Name(String),
}
impl Filter {
    fn node_list(&self) -> bool {
        matches!(self, Self::ChildNodes | Self::Name(_))
    }
}
#[derive(Clone, Hash, Eq, PartialEq)]
struct Query {
    root: NodeId,
    filter: Filter,
}
#[derive(Default)]
struct Cache {
    revision: u64,
    retained: usize,
    reflection_work: usize,
    last_tag: Option<(Query, u64, Arc<[NodeId]>)>,
    matches: VecDeque<(Query, Arc<[NodeId]>)>,
}
struct Collection {
    dom: Weak<RefCell<State>>,
    query: Query,
    cache: Rc<RefCell<Cache>>,
    _allocation: QueryAllocation,
}
struct QueryAllocation {
    resident: Rc<Cell<usize>>,
    bytes: usize,
}
impl Drop for QueryAllocation {
    fn drop(&mut self) {
        self.resident.set(self.resident.get() - self.bytes);
    }
}

impl Collection {
    fn nodes(&self, mut vm: Option<&mut Vm>) -> Result<Arc<[NodeId]>, JsError> {
        let state = self
            .dom
            .upgrade()
            .ok_or_else(|| JsError::type_error("Collection document is gone"))?;
        let state = state.borrow();
        let mut cache = self.cache.borrow_mut();
        if cache.revision != state.revision {
            cache.revision = state.revision;
            cache.retained = 0;
            cache.matches.clear();
        }
        if let Some(index) = cache
            .matches
            .iter()
            .position(|(query, _)| *query == self.query)
        {
            let hit = cache.matches.remove(index).unwrap();
            let nodes = hit.1.clone();
            cache.matches.push_front(hit);
            return Ok(nodes);
        }
        if let Some(updated) = self.updated_tag(&state, &mut cache, &mut vm)? {
            return Ok(updated);
        }
        let document = &state.document;
        let root = self.query.root;
        charge_scan(&mut cache, &mut vm, document.nodes[root.0].children.len())?;
        let nodes: Arc<[NodeId]> = match &self.query.filter {
            Filter::Children | Filter::ChildNodes => document.nodes[root.0]
                .children
                .iter()
                .copied()
                .filter(|id| {
                    matches!(self.query.filter, Filter::ChildNodes) || !document.tag(*id).is_empty()
                })
                .collect(),
            filter => {
                let html_document = document.is_html_document(document.node_document(root));
                let quirks = document.node_document(root) == document.root
                    && document.quirks_mode == QuirksMode::Quirks;
                let mut matches = Vec::new();
                let mut pending: Vec<_> = document.nodes[root.0]
                    .children
                    .iter()
                    .rev()
                    .copied()
                    .collect();
                while let Some(id) = pending.pop() {
                    let children = &document.nodes[id.0].children;
                    charge_scan(&mut cache, &mut vm, children.len())?;
                    pending.extend(children.iter().rev().copied());
                    let tag = document.tag(id);
                    if tag.is_empty() {
                        continue;
                    }
                    let matched = match filter {
                        Filter::Tag { original, lower } => {
                            original == "*"
                                || if html_document && document.namespace(id) == Namespace::Html {
                                    tag == lower
                                } else {
                                    tag == original
                                }
                        }
                        Filter::Namespace { namespace, local } => {
                            (local == "*" || document.local_name(id) == local)
                                && (namespace.as_deref() == Some("*")
                                    || document.namespace_uri(id) == namespace.as_deref())
                        }
                        Filter::Classes(classes) => {
                            if let Some(value) = document.attr(id, "class")
                                && !classes.is_empty()
                            {
                                charge_scan(&mut cache, &mut vm, value.len())?;
                                class_matches(classes, value, quirks)
                            } else {
                                false
                            }
                        }
                        Filter::Name(name) => {
                            document.namespace(id) == Namespace::Html
                                && document.attr(id, "name") == Some(name)
                        }
                        _ => unreachable!(),
                    };
                    if matched {
                        matches.push(id);
                    }
                }
                matches.into()
            }
        };
        if root == document.root && matches!(self.query.filter, Filter::Tag { .. }) {
            cache.last_tag = (nodes.len() <= 4096)
                .then(|| (self.query.clone(), state.tag_tree_revision, nodes.clone()));
        }
        if nodes.len() > CACHE_NODES {
            return Ok(nodes);
        }
        while cache.matches.len() >= CACHE_ENTRIES || cache.retained + nodes.len() > CACHE_NODES {
            let Some((_, discarded)) = cache.matches.pop_back() else {
                break;
            };
            cache.retained -= discarded.len();
        }
        cache.retained += nodes.len();
        cache
            .matches
            .push_front((self.query.clone(), nodes.clone()));
        Ok(nodes)
    }

    fn updated_tag(
        &self,
        state: &State,
        cache: &mut Cache,
        vm: &mut Option<&mut Vm>,
    ) -> Result<Option<Arc<[NodeId]>>, JsError> {
        let Filter::Tag { original, lower } = &self.query.filter else {
            return Ok(None);
        };
        let Some((query, revision, nodes)) = &cache.last_tag else {
            return Ok(None);
        };
        if query != &self.query {
            return Ok(None);
        }
        if *revision == state.tag_tree_revision {
            return Ok(Some(nodes.clone()));
        }
        if revision.wrapping_add(1) != state.tag_tree_revision {
            return Ok(None);
        }
        let Some((parent, child)) = state.appended_leaf else {
            return Ok(None);
        };
        let doc = &state.document;
        let old = nodes.clone();
        let tag = doc.tag(child);
        let matched = !tag.is_empty()
            && (original == "*"
                || if doc.is_html_document(doc.root) && doc.namespace(child) == Namespace::Html {
                    tag == lower
                } else {
                    tag == original
                });
        let mut current = Some(parent);
        let mut connected = false;
        while matched && let Some(id) = current {
            charge_scan(cache, vm, 1)?;
            if id == doc.root {
                connected = true;
                break;
            }
            current = doc.nodes[id.0].parent;
        }
        let updated = if connected {
            // Otherwise a matching node elsewhere could follow this insertion.
            if old.len() >= 4096
                || old
                    .last()
                    .is_some_and(|n| doc.nodes[n.0].parent != Some(parent))
            {
                return Ok(None);
            }
            charge_scan(cache, vm, old.len() + 1)?;
            let mut next = Vec::with_capacity(old.len() + 1);
            next.extend_from_slice(&old);
            next.push(child);
            next.into()
        } else {
            old
        };
        cache.last_tag = Some((self.query.clone(), state.tag_tree_revision, updated.clone()));
        Ok(Some(updated))
    }
    fn get(&self, vm: &mut Vm, index: usize) -> Result<Option<Value>, JsError> {
        let list = self.nodes(Some(vm))?;
        let Some(&id) = list.get(index) else {
            return Ok(None);
        };
        let state = self
            .dom
            .upgrade()
            .ok_or_else(|| JsError::type_error("Collection document is gone"))?;
        wrap(vm, &state, Some(id)).map(Some)
    }
}

fn charge_scan(cache: &mut Cache, vm: &mut Option<&mut Vm>, units: usize) -> Result<(), JsError> {
    if let Some(vm) = vm.as_deref_mut() {
        return vm.charge_work(units);
    }
    // The VM's pure metadata path cannot debit a VM task. Cap uncached
    // reflective scans for this realm while allowing O(1) cache hits.
    if cache.reflection_work.saturating_add(units) > MAX_REFLECTION_SCAN_WORK {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Live collection reflection work budget exceeded",
        ));
    }
    cache.reflection_work += units;
    Ok(())
}

fn class_matches(classes: &[String], value: &str, quirks: bool) -> bool {
    let mut seen = 0u128;
    let mut remaining = classes.len();
    for name in value.split_ascii_whitespace() {
        if let Ok(index) = classes.binary_search_by(|class| {
            if quirks {
                class
                    .bytes()
                    .cmp(name.bytes().map(|byte| byte.to_ascii_lowercase()))
            } else {
                class.as_str().cmp(name)
            }
        }) {
            let bit = 1u128 << index;
            if seen & bit == 0 {
                seen |= bit;
                remaining -= 1;
                if remaining == 0 {
                    return true;
                }
            }
        }
    }
    false
}

impl HostIndexedProperties for Collection {
    fn indexed_len(&self) -> Result<usize, JsError> {
        self.nodes(None).map(|nodes| nodes.len())
    }
    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        self.get(vm, index as usize)
    }
}

#[derive(Clone)]
struct Host {
    state: Shared,
    collection: Value,
    node_list: Value,
    cache: Rc<RefCell<Cache>>,
    child_view_bytes: Rc<Cell<usize>>,
    resident: Rc<Cell<usize>>,
}
impl Host {
    fn new_value(
        &self,
        vm: &mut Vm,
        query: Query,
        owner: Option<(Value, u32)>,
    ) -> Result<Value, JsError> {
        if let Some((owner, slot)) = &owner
            && let Some(view) = vm.host_slot(owner, *slot)
        {
            return Ok(view);
        }
        let bytes = 256
            + match &query.filter {
                Filter::Tag { original, lower } => original.capacity() + lower.capacity(),
                Filter::Namespace { namespace, local } => {
                    namespace.as_ref().map_or(0, String::capacity) + local.capacity()
                }
                Filter::Classes(names) => {
                    names.capacity() * std::mem::size_of::<String>()
                        + names.iter().map(String::capacity).sum::<usize>()
                }
                Filter::Name(name) => name.capacity(),
                Filter::Children | Filter::ChildNodes => 0,
            };
        let (resident, limit) = if owner.is_some() {
            (&self.child_view_bytes, MAX_CHILD_VIEW_BYTES)
        } else {
            (&self.resident, MAX_LIVE_QUERY_BYTES)
        };
        if resident.get() + bytes > limit {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Retained live collection query budget exceeded",
            ));
        }
        resident.set(resident.get() + bytes);
        let allocation = QueryAllocation {
            resident: resident.clone(),
            bytes,
        };
        let value = vm.new_object_with_prototype(if query.filter.node_list() {
            self.node_list.clone()
        } else {
            self.collection.clone()
        })?;
        let provider = Rc::new(Collection {
            dom: Rc::downgrade(&self.state),
            query: query.clone(),
            cache: self.cache.clone(),
            _allocation: allocation,
        });
        // Charge the initial traversal in the creating task, even if script
        // only uses reflective presence checks on the returned object.
        provider.nodes(Some(vm))?;
        vm.set_host_data(value.clone(), provider.clone())?;
        vm.set_host_indexed_properties(value.clone(), provider)?;
        if let Some((owner, slot)) = owner {
            vm.set_host_slot(owner, slot, value.clone())?;
        }
        Ok(value)
    }
}

struct StaticList {
    dom: Weak<RefCell<State>>,
    nodes: Rc<[NodeId]>,
    _allocation: QueryAllocation,
}
impl HostIndexedProperties for StaticList {
    fn indexed_len(&self) -> Result<usize, JsError> {
        Ok(self.nodes.len())
    }

    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError> {
        let Some(&id) = self.nodes.get(index as usize) else {
            return Ok(None);
        };
        let state = self
            .dom
            .upgrade()
            .ok_or_else(|| JsError::type_error("Collection document is gone"))?;
        wrap(vm, &state, Some(id)).map(Some)
    }
}

#[derive(Clone)]
pub(super) struct StaticNodeLists {
    dom: Weak<RefCell<State>>,
    prototype: Value,
    resident: Rc<Cell<usize>>,
}
impl StaticNodeLists {
    pub(super) fn create(&self, vm: &mut Vm, nodes: Rc<[NodeId]>) -> Result<Value, JsError> {
        let bytes = 128 + nodes.len() * std::mem::size_of::<NodeId>();
        if self.resident.get() + bytes > MAX_STATIC_LIST_BYTES {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Retained static NodeList budget exceeded",
            ));
        }
        let value = vm.new_object_with_prototype(self.prototype.clone())?;
        self.resident.set(self.resident.get() + bytes);
        let list = Rc::new(StaticList {
            dom: self.dom.clone(),
            nodes,
            _allocation: QueryAllocation {
                resident: self.resident.clone(),
                bytes,
            },
        });
        vm.set_host_data(value.clone(), list.clone())?;
        vm.set_host_indexed_properties(value.clone(), list)?;
        Ok(value)
    }
}

enum Receiver {
    Live(Rc<Collection>),
    Static(Rc<StaticList>),
}
impl Receiver {
    fn len(&self, vm: &mut Vm) -> Result<usize, JsError> {
        match self {
            Self::Live(view) => view.nodes(Some(vm)).map(|nodes| nodes.len()),
            Self::Static(view) => view.indexed_len(),
        }
    }
    fn get(&self, vm: &mut Vm, index: usize) -> Result<Option<Value>, JsError> {
        match self {
            Self::Live(view) => view.get(vm, index),
            Self::Static(view) => view.indexed_get(vm, index as u32),
        }
    }
    fn is_node_list(&self) -> bool {
        match self {
            Self::Live(view) => view.query.filter.node_list(),
            Self::Static(_) => true,
        }
    }
}
fn receiver(vm: &Vm, this: &Value) -> Result<Receiver, JsError> {
    vm.host_data::<Collection>(this)
        .map(Receiver::Live)
        .or_else(|| vm.host_data::<StaticList>(this).map(Receiver::Static))
        .ok_or_else(|| JsError::type_error("Illegal collection receiver"))
}
fn input(vm: &mut Vm, value: Value) -> Result<String, JsError> {
    let value = text(vm, value)?;
    if value.len() > MAX_COLLECTION_QUERY_BYTES {
        return Err(JsError::new(
            ErrorKind::HeapLimit,
            "DOM collection query exceeds 16 KiB",
        ));
    }
    Ok(value)
}
fn count_arg(args: &[Value], count: usize) -> Result<(), JsError> {
    if args.len() >= count {
        Ok(())
    } else {
        Err(JsError::type_error("Missing required collection argument"))
    }
}

pub(super) fn install(
    vm: &mut Vm,
    state: &Shared,
    node_prototype: Value,
    document: Value,
) -> Result<StaticNodeLists, JsError> {
    let collection = vm.new_object();
    let node_list = vm.new_object();
    let array = vm.get_global("Array")?;
    let array = vm.get_property(array, "prototype")?;
    let values = vm.get_property(array.clone(), "values")?;
    let iterator = vm.to_property_key(
        &vm.well_known_symbol("iterator")
            .expect("VM iterator symbol"),
    )?;
    let tag = vm.to_property_key(&vm.well_known_symbol("toStringTag").expect("VM tag symbol"))?;
    for (name, prototype) in [
        ("HTMLCollection", collection.clone()),
        ("NodeList", node_list.clone()),
    ] {
        let constructor = vm.new_native_constructor(name, |_, _, _, _| {
            Err(JsError::type_error("Illegal constructor"))
        });
        vm.define_data_property(
            constructor.clone(),
            "prototype",
            prototype.clone(),
            false,
            false,
            false,
        )?;
        vm.define_data_property(
            prototype.clone(),
            "constructor",
            constructor.clone(),
            true,
            false,
            true,
        )?;
        vm.define_data_property_key(
            prototype.clone(),
            tag.clone(),
            name.into(),
            false,
            false,
            true,
        )?;
        vm.define_data_property_key(
            prototype.clone(),
            iterator.clone(),
            values.clone(),
            true,
            false,
            true,
        )?;
        vm.set_global(name, constructor)?;
        vm.root(prototype.clone());
        getter(vm, prototype.clone(), "length", |vm, this, _| {
            let view = receiver(vm, &this)?;
            Ok((view.len(vm)? as f64).into())
        })?;
        method(vm, prototype, "item", |vm, this, args| {
            count_arg(args, 1)?;
            let view = receiver(vm, &this)?;
            let n = vm.to_number(&args[0])?.trunc();
            let index = if n.is_finite() {
                n.rem_euclid(4294967296.) as usize
            } else {
                0
            };
            Ok(view.get(vm, index)?.unwrap_or(Value::Null))
        })?;
    }
    method(vm, collection.clone(), "namedItem", |vm, this, args| {
        count_arg(args, 1)?;
        let Receiver::Live(view) = receiver(vm, &this)? else {
            return Err(JsError::type_error("HTMLCollection receiver required"));
        };
        if view.query.filter.node_list() {
            return Err(JsError::type_error("HTMLCollection receiver required"));
        }
        let name = input(vm, args[0].clone())?;
        if name.is_empty() {
            return Ok(Value::Null);
        }
        let list = view.nodes(Some(vm))?;
        vm.charge_work(list.len())?;
        let state = view
            .dom
            .upgrade()
            .ok_or_else(|| JsError::type_error("Collection document is gone"))?;
        let id = {
            let data = state.borrow();
            let doc = &data.document;
            list.iter().copied().find(|&id| {
                doc.attr(id, "id") == Some(&name)
                    || (doc.namespace(id) == Namespace::Html && doc.attr(id, "name") == Some(&name))
            })
        };
        wrap(vm, &state, id)
    })?;
    for name in ["keys", "values", "entries"] {
        let callable = vm.get_property(array.clone(), name)?;
        vm.define_data_property(node_list.clone(), name, callable, true, false, true)?;
    }
    method(vm, node_list.clone(), "forEach", |vm, this, args| {
        count_arg(args, 1)?;
        let view = receiver(vm, &this)?;
        if !view.is_node_list() || !vm.is_callable(&args[0]) {
            return Err(JsError::type_error(
                "NodeList and a callable callback are required",
            ));
        }
        let len = view.len(vm)?;
        for i in 0..len {
            vm.charge_work(1)?;
            if let Some(node) = view.get(vm, i)? {
                vm.call(
                    args[0].clone(),
                    arg(args, 1),
                    &[node, (i as f64).into(), this.clone()],
                )?;
            }
        }
        Ok(Value::Undefined)
    })?;
    let host = Host {
        state: state.clone(),
        collection,
        node_list,
        cache: Rc::default(),
        child_view_bytes: Rc::default(),
        resident: Rc::default(),
    };
    for (name, filter, slot) in [
        ("children", Filter::Children, CHILDREN_SLOT),
        ("childNodes", Filter::ChildNodes, CHILD_NODES_SLOT),
    ] {
        let host = host.clone();
        getter(vm, node_prototype.clone(), name, move |vm, this, _| {
            let root = node(&host.state, &this)?;
            host.new_value(
                vm,
                Query {
                    root,
                    filter: filter.clone(),
                },
                Some((this, slot)),
            )
        })?;
    }
    for name in [
        "getElementsByTagName",
        "getElementsByTagNameNS",
        "getElementsByClassName",
        "getElementsByName",
    ] {
        let host = host.clone();
        let target = if name == "getElementsByName" {
            document.clone()
        } else {
            node_prototype.clone()
        };
        method(vm, target, name, move |vm, this, args| {
            count_arg(
                args,
                if name == "getElementsByTagNameNS" {
                    2
                } else {
                    1
                },
            )?;
            let root = node(&host.state, &this)?;
            let filter = match name {
                "getElementsByTagNameNS" => {
                    let namespace = if args[0].is_nullish() {
                        None
                    } else {
                        Some(input(vm, args[0].clone())?).filter(|namespace| !namespace.is_empty())
                    };
                    Filter::Namespace {
                        namespace,
                        local: input(vm, args[1].clone())?,
                    }
                }
                "getElementsByClassName" => {
                    let class = input(vm, args[0].clone())?;
                    let quirks = host.state.borrow().document.quirks_mode == QuirksMode::Quirks;
                    let mut tokens = class
                        .split_ascii_whitespace()
                        .map(|part| {
                            if quirks {
                                part.to_ascii_lowercase()
                            } else {
                                part.to_owned()
                            }
                        })
                        .collect::<Vec<_>>();
                    tokens.sort_unstable();
                    tokens.dedup();
                    if tokens.len() > 128 {
                        return Err(unsupported("Class-name collection token limit exceeded"));
                    }
                    Filter::Classes(tokens)
                }
                "getElementsByName" => {
                    if !host.state.borrow().document.is_document(root) {
                        return Err(JsError::type_error("Document receiver required"));
                    }
                    Filter::Name(input(vm, args[0].clone())?)
                }
                _ => {
                    let original = input(vm, args[0].clone())?;
                    let lower = original.to_ascii_lowercase();
                    Filter::Tag { original, lower }
                }
            };
            host.new_value(vm, Query { root, filter }, None)
        })?;
    }
    Ok(StaticNodeLists {
        dom: Rc::downgrade(state),
        prototype: host.node_list,
        resident: Rc::default(),
    })
}

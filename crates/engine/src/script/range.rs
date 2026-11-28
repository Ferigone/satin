//! Document ranges and contextual parsing over Satin's DOM arena.
use super::{
    Shared, arg, detach, document_node, dynamic_scripts, mark_dirty, mutation_observers,
    set_text,
    dom_api::{self, Errors},
    getter, node, selection, text, webidl, wrap,
};
use crate::dom::{Document, NodeId, NodeKind};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{cell::Cell, cmp::Ordering, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Point {
    pub node: NodeId,
    pub offset: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Bounds {
    pub start: Point,
    pub end: Point,
}

struct Range(Cell<Bounds>);

fn range(vm: &Vm, receiver: &Value) -> Result<Rc<Range>, JsError> {
    vm.host_data(receiver)
        .ok_or_else(|| JsError::type_error("Illegal Range receiver"))
}

fn new_range(vm: &mut Vm, object: Value, bounds: Bounds) -> Result<Value, JsError> {
    vm.mark_platform_object(&object)?;
    vm.set_host_data(object.clone(), Rc::new(Range(Cell::new(bounds))))?;
    Ok(object)
}

pub(super) fn create(vm: &mut Vm, prototype: Value, bounds: Bounds) -> Result<Value, JsError> {
    let object = vm.new_object_with_prototype(prototype)?;
    new_range(vm, object, bounds)
}

pub(super) fn bounds(vm: &Vm, value: &Value) -> Result<Bounds, JsError> {
    Ok(range(vm, value)?.0.get())
}

fn update_bounds(vm: &mut Vm, host: &Shared, receiver: &Value, range: &Range, bounds: Bounds) {
    if range.0.replace(bounds) != bounds {
        selection::range_changed(vm, host, receiver);
    }
}

fn collapsed(node: NodeId) -> Bounds {
    let point = Point { node, offset: 0 };
    Bounds {
        start: point,
        end: point,
    }
}

fn ancestry(vm: &mut Vm, doc: &Document, mut node: NodeId) -> Result<Vec<NodeId>, JsError> {
    let mut result = vec![node];
    while let Some(parent) = doc.nodes[node.0].parent {
        vm.charge_work(1)?;
        result.push(parent);
        node = parent;
    }
    result.reverse();
    Ok(result)
}

fn child_offset(
    vm: &mut Vm,
    doc: &Document,
    parent: NodeId,
    child: NodeId,
) -> Result<usize, JsError> {
    let children = &doc.nodes[parent.0].children;
    vm.charge_buffer_work(children.len())?;
    Ok(children
        .iter()
        .position(|&node| node == child)
        .expect("DOM parent owns child"))
}

pub(super) fn compare(
    vm: &mut Vm,
    doc: &Document,
    left: Point,
    right: Point,
) -> Result<Option<Ordering>, JsError> {
    if left.node == right.node {
        return Ok(Some(left.offset.cmp(&right.offset)));
    }
    let a = ancestry(vm, doc, left.node)?;
    let b = ancestry(vm, doc, right.node)?;
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let result = match (a.get(common), b.get(common)) {
        (None, Some(child)) => {
            if left.offset <= child_offset(vm, doc, left.node, *child)? {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(child), None) => {
            if right.offset <= child_offset(vm, doc, right.node, *child)? {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(a), Some(b)) if common > 0 => {
            let parent = doc.nodes[a.0].parent.expect("Common parent");
            child_offset(vm, doc, parent, *a)?.cmp(&child_offset(vm, doc, parent, *b)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

pub(super) fn node_length(vm: &mut Vm, doc: &Document, node: NodeId) -> Result<usize, JsError> {
    let node = &doc.nodes[node.0];
    if let Some(text) = node.kind.character_data() {
        vm.charge_buffer_work(text.len())?;
        return Ok(text.encode_utf16().count());
    }
    Ok(node.children.len())
}

fn adjusted_bounds(
    vm: &mut Vm,
    doc: &Document,
    mut bounds: Bounds,
    point: Point,
    start: bool,
) -> Result<Bounds, JsError> {
    if start {
        if compare(vm, doc, point, bounds.end)?.is_none_or(|order| order.is_gt()) {
            bounds.end = point;
        }
        bounds.start = point;
    } else {
        if compare(vm, doc, point, bounds.start)?.is_none_or(|order| order.is_lt()) {
            bounds.start = point;
        }
        bounds.end = point;
    }
    Ok(bounds)
}

/// Preflight the nodes and character-data edits before changing the arena.
/// Fully contained subtrees are removed as units; partially contained
/// ancestors survive and only their selected descendants are removed.
enum Deletion {
    Remove { parent: NodeId, child: NodeId },
    Text { node: NodeId, value: String },
}

fn plan_deletion(
    vm: &mut Vm,
    doc: &Document,
    bounds: Bounds,
    errors: &Errors,
) -> Result<(Point, Vec<Deletion>), JsError> {
    let start = bounds.start;
    let end = bounds.end;
    if start.offset > node_length(vm, doc, start.node)?
        || end.offset > node_length(vm, doc, end.node)?
        || compare(vm, doc, start, end)?.is_none_or(|order| order.is_gt())
    {
        return Err(errors.exception(vm, "InvalidStateError", "Invalid range boundaries"));
    }
    let a = ancestry(vm, doc, start.node)?;
    let b = ancestry(vm, doc, end.node)?;
    let common_count = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let common = a[common_count - 1];
    let collapse = if common == start.node {
        start
    } else {
        Point {
            node: common,
            offset: child_offset(vm, doc, common, a[common_count])? + 1,
        }
    };
    let mut operations = Vec::new();
    let mut pending = vec![common];
    while let Some(id) = pending.pop() {
        vm.charge_work(1)?;
        let entry = &doc.nodes[id.0];
        if id != common {
            let parent = entry.parent.expect("Range child has a parent");
            let index = child_offset(vm, doc, parent, id)?;
            let before = Point { node: parent, offset: index };
            let after = Point { node: parent, offset: index + 1 };
            if compare(vm, doc, start, before)?.is_some_and(|order| !order.is_gt())
                && compare(vm, doc, after, end)?.is_some_and(|order| !order.is_gt())
            {
                // detach scans the sibling vector; charge its original length
                // for each removal before applying any edits.
                vm.charge_buffer_work(doc.nodes[parent.0].children.len())?;
                operations.push(Deletion::Remove { parent, child: id });
                continue;
            }
        }
        if let Some(data) = entry.kind.character_data() {
            let length = node_length(vm, doc, id)?;
            let from = if id == start.node { start.offset } else { 0 };
            let to = if id == end.node { end.offset } else { length };
            if from < to {
                vm.charge_buffer_work(data.len().saturating_mul(4))?;
                let mut units: Vec<u16> = data.encode_utf16().collect();
                units.drain(from..to);
                operations.push(Deletion::Text {
                    node: id,
                    value: String::from_utf16_lossy(&units),
                });
            }
            continue;
        }
        // Queue children in reverse so the actual removals are in tree order.
        vm.charge_buffer_work(entry.children.len())?;
        for (index, &child) in entry.children.iter().enumerate().rev() {
            let before = Point { node: id, offset: index };
            let after = Point { node: id, offset: index + 1 };
            if compare(vm, doc, after, start)?.is_some_and(|order| !order.is_gt())
                || compare(vm, doc, before, end)?.is_some_and(|order| !order.is_lt())
            {
                continue;
            }
            pending.push(child);
        }
    }
    Ok((collapse, operations))
}

pub(super) fn install(vm: &mut Vm, host: &Shared, errors: &Errors) -> Result<Value, JsError> {
    let prototype = vm.new_object();
    let intrinsic = prototype.clone();
    let owner = host.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "Range",
        0,
        move |vm, _, _, target| {
            let target = target.ok_or_else(|| JsError::type_error("Range requires new"))?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            new_range(vm, object, collapsed(owner.borrow().document.root))
        },
    )?;
    let owner = host.clone();
    let intrinsic = prototype.clone();
    let document_proto = host.borrow().document_prototype.clone();
    webidl::operation(
        vm,
        document_proto,
        "createRange",
        0,
        move |vm, document, _| {
            let id = document_node(&owner, &document)?;
            create(vm, intrinsic.clone(), collapsed(id))
        },
    )?;
    for (name, value) in [
        ("START_TO_START", 0),
        ("START_TO_END", 1),
        ("END_TO_END", 2),
        ("END_TO_START", 3),
    ] {
        for target in [&constructor, &prototype] {
            vm.define_data_property(target.clone(), name, value.into(), false, true, false)?;
        }
    }
    for start in [true, false] {
        let owner = host.clone();
        let name = if start {
            "startContainer"
        } else {
            "endContainer"
        };
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            wrap(
                vm,
                &owner,
                Some(if start {
                    bounds.start.node
                } else {
                    bounds.end.node
                }),
            )
        })?;
        let name = if start { "startOffset" } else { "endOffset" };
        getter(vm, prototype.clone(), name, move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            Ok((if start {
                bounds.start.offset
            } else {
                bounds.end.offset
            })
            .into())
        })?;
        let owner = host.clone();
        let errors = errors.clone();
        let name = if start { "setStart" } else { "setEnd" };
        webidl::operation(vm, prototype.clone(), name, 2, move |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            webidl::required(args, 2, name)?;
            let node = node(&owner, &args[0])?;
            let offset = vm.to_number(&args[1])?.trunc().rem_euclid(4_294_967_296.0) as usize;
            let state = owner.borrow();
            let doc = &state.document;
            if matches!(doc.nodes[node.0].kind, NodeKind::Doctype { .. }) {
                return Err(errors.exception(
                    vm,
                    "InvalidNodeTypeError",
                    "A doctype cannot be a boundary container",
                ));
            }
            if offset > node_length(vm, doc, node)? {
                return Err(errors.exception(
                    vm,
                    "IndexSizeError",
                    "Range offset exceeds the node length",
                ));
            }
            let bounds = adjusted_bounds(vm, doc, range.0.get(), Point { node, offset }, start)?;
            drop(state);
            update_bounds(vm, &owner, &receiver, &range, bounds);
            Ok(Value::Undefined)
        })?;
    }
    getter(vm, prototype.clone(), "collapsed", |vm, receiver, _| {
        let bounds = range(vm, &receiver)?.0.get();
        Ok((bounds.start == bounds.end).into())
    })?;
    let owner = host.clone();
    getter(
        vm,
        prototype.clone(),
        "commonAncestorContainer",
        move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            let common = {
                let state = owner.borrow();
                let a = ancestry(vm, &state.document, bounds.start.node)?;
                let b = ancestry(vm, &state.document, bounds.end.node)?;
                a.into_iter()
                    .zip(b)
                    .take_while(|(x, y)| x == y)
                    .last()
                    .map(|(node, _)| node)
            };
            wrap(vm, &owner, common)
        },
    )?;
    let owner = host.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "collapse",
        0,
        move |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            let bounds = range.0.get();
            let point = if arg(args, 0).is_truthy() {
                bounds.start
            } else {
                bounds.end
            };
            let bounds = Bounds {
                start: point,
                end: point,
            };
            update_bounds(vm, &owner, &receiver, &range, bounds);
            Ok(Value::Undefined)
        },
    )?;
    let intrinsic = prototype.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "cloneRange",
        0,
        move |vm, receiver, _| {
            let bounds = range(vm, &receiver)?.0.get();
            create(vm, intrinsic.clone(), bounds)
        },
    )?;
    for contents in [false, true] {
        let owner = host.clone();
        let errors = errors.clone();
        let name = if contents {
            "selectNodeContents"
        } else {
            "selectNode"
        };
        webidl::operation(vm, prototype.clone(), name, 1, move |vm, receiver, args| {
            let range = range(vm, &receiver)?;
            webidl::required(args, 1, name)?;
            let node = node(&owner, &args[0])?;
            let state = owner.borrow();
            let doc = &state.document;
            let (container, start, end) = if contents {
                if matches!(doc.nodes[node.0].kind, NodeKind::Doctype { .. }) {
                    return Err(errors.exception(
                        vm,
                        "InvalidNodeTypeError",
                        "A doctype cannot contain a range",
                    ));
                }
                (node, 0, node_length(vm, doc, node)?)
            } else {
                let parent = doc.nodes[node.0].parent.ok_or_else(|| {
                    errors.exception(vm, "InvalidNodeTypeError", "Range node has no parent")
                })?;
                let index = child_offset(vm, doc, parent, node)?;
                (parent, index, index + 1)
            };
            let bounds = Bounds {
                start: Point {
                    node: container,
                    offset: start,
                },
                end: Point {
                    node: container,
                    offset: end,
                },
            };
            drop(state);
            update_bounds(vm, &owner, &receiver, &range, bounds);
            Ok(Value::Undefined)
        })?;
    }
    let owner = host.clone();
    let delete_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "deleteContents",
        0,
        move |vm, receiver, _| {
            let range = range(vm, &receiver)?;
            let bounds = range.0.get();
            if bounds.start == bounds.end {
                return Ok(Value::Undefined);
            }
            let (point, operations) = {
                let state = owner.borrow();
                plan_deletion(vm, &state.document, bounds, &delete_errors)?
            };
            let mut affected = Vec::new();
            for operation in operations {
                match operation {
                    Deletion::Remove { parent, child } => {
                        let mut state = owner.borrow_mut();
                        if state.document.nodes[child.0].parent == Some(parent) {
                            mutation_observers::removing(&state, parent, child, false);
                            detach(&mut state.document, child);
                            mark_dirty(&mut state);
                            affected.push(parent);
                        }
                    }
                    Deletion::Text { node, value } => {
                        let parent = owner.borrow().document.nodes[node.0].parent;
                        set_text(vm, &owner, node, value)?;
                        affected.extend(parent);
                    }
                }
            }
            update_bounds(vm, &owner, &receiver, &range, Bounds { start: point, end: point });
            mutation_observers::schedule(vm, &owner);
            dynamic_scripts::post_connection(vm, &owner, affected)?;
            Ok(Value::Undefined)
        },
    )?;
    let owner = host.clone();
    let compare_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "compareBoundaryPoints",
        2,
        move |vm, receiver, args| {
            let bounds = range(vm, &receiver)?.0.get();
            webidl::required(args, 2, "Range.compareBoundaryPoints")?;
            let how = vm.to_number(&args[0])?.trunc().rem_euclid(65536.0) as u16;
            let source = range(vm, &args[1])?.0.get();
            let (a, b) = match how {
                0 => (bounds.start, source.start),
                1 => (bounds.end, source.start),
                2 => (bounds.end, source.end),
                3 => (bounds.start, source.end),
                _ => {
                    return Err(compare_errors.exception(
                        vm,
                        "NotSupportedError",
                        "Unknown range comparison mode",
                    ));
                }
            };
            let doc = &owner.borrow().document;
            match compare(vm, doc, a, b)? {
                Some(Ordering::Less) => Ok((-1).into()),
                Some(Ordering::Equal) => Ok(0.into()),
                Some(Ordering::Greater) => Ok(1.into()),
                None => Err(compare_errors.exception(
                    vm,
                    "WrongDocumentError",
                    "Range boundaries have different roots",
                )),
            }
        },
    )?;
    let owner = host.clone();
    let fragment_errors = errors.clone();
    webidl::operation(
        vm,
        prototype.clone(),
        "createContextualFragment",
        1,
        move |vm, receiver, args| {
            let start = range(vm, &receiver)?.0.get().start.node;
            webidl::required(args, 1, "Range.createContextualFragment")?;
            let source = text(vm, args[0].clone())?;
            let context = {
                let state = owner.borrow();
                match &state.document.nodes[start.0].kind {
                    NodeKind::Element { .. } => start,
                    _ => state.document.nodes[start.0].parent.unwrap_or(start),
                }
            };
            dom_api::contextual_fragment(vm, &owner, context, &source, &fragment_errors)
        },
    )?;
    Ok(prototype)
}

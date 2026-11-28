//! Autonomous HTML custom elements in the document's default registry.
use super::{
    ConsolePhase, Shared, State, arg, dom_api::Errors, new_detached, node, report_error, text,
    webidl, wrap,
};
use crate::dom::{AttributeNamespace, Document, Namespace, NodeId, NodeKind};
use rust_browser_javascript::{ErrorKind, JsError, ObjectId, PromiseCapability, Value, Vm};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

const REGISTRY_SLOT: u32 = 0x4355_5354;
const REFERENCES_SLOT: u32 = 0x4355_5246;

struct Definition {
    name: String,
    constructor: Value,
    connected: Option<Value>,
    disconnected: Option<Value>,
    changed: Option<Value>,
    observed: HashSet<String>,
    disable_shadow: bool,
    // An empty entry means super() has already consumed this upgrade candidate.
    construction: RefCell<Vec<Construction>>,
}

enum Construction {
    Upgrade(Option<NodeId>),
    Fresh(Option<NodeId>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Building,
    Custom,
    Failed,
}

#[derive(Default)]
pub(super) struct Registry {
    definitions: HashMap<String, Rc<Definition>>,
    by_constructor: HashMap<ObjectId, Rc<Definition>>,
    pending: HashMap<String, PromiseCapability>,
    elements: HashMap<NodeId, Status>,
    reactions: Vec<Reaction>,
    defining: bool,
    observed_bytes: usize,
}

pub(super) enum Reaction {
    Upgrade(NodeId),
    Call(NodeId, Value, Vec<Value>),
}

pub(super) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.contains('-')
        && !matches!(
            name,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
        && chars.all(|c| {
            matches!(c, '-' | '.' | '0'..='9' | '_' | 'a'..='z' | '\u{b7}' |
            '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{37d}' |
            '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{203f}'..='\u{2040}' |
            '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' |
            '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
        })
}

fn definition(state: &State, id: NodeId) -> Option<Rc<Definition>> {
    if state.document.namespace(id) != Namespace::Html {
        return None;
    }
    state
        .custom_elements
        .borrow()
        .definitions
        .get(state.document.tag(id))
        .cloned()
}

fn fatal(error: &JsError) -> bool {
    matches!(
        error.kind,
        ErrorKind::HeapLimit
            | ErrorKind::ExecutionLimit
            | ErrorKind::InternalError
            | ErrorKind::UnsupportedFeature
    )
}

fn report(host: &Shared, name: &str, error: JsError) -> Result<(), JsError> {
    if fatal(&error) {
        return Err(error);
    }
    report_error(host, ConsolePhase::HostTask, Some(name), &error, || {
        format!("Custom element {name}: {error}")
    });
    Ok(())
}

fn status(host: &Shared, id: NodeId, value: Status) {
    host.borrow()
        .custom_elements
        .borrow_mut()
        .elements
        .insert(id, value);
}

fn bare(host: &Shared, name: String) -> Result<NodeId, JsError> {
    new_detached(
        host,
        NodeKind::Element {
            tag: name,
            attrs: Default::default(),
        },
    )
}

pub(super) fn html_constructor(
    vm: &mut Vm,
    host: &Shared,
    target: Option<Value>,
) -> Result<Value, JsError> {
    let target = target.ok_or_else(|| JsError::type_error("HTMLElement requires new"))?;
    let definition = target
        .as_object()
        .and_then(|id| {
            host.borrow()
                .custom_elements
                .borrow()
                .by_constructor
                .get(&id)
                .cloned()
        })
        .ok_or_else(|| JsError::type_error("Illegal HTMLElement constructor"))?;
    let proto = vm
        .get_property(target, "prototype")?
        .as_object()
        .or_else(|| host.borrow().html_element_prototype.as_object());
    let (existing, fresh) = match definition.construction.borrow_mut().last_mut() {
        Some(Construction::Upgrade(entry)) => {
            let id = entry.take().ok_or_else(|| {
                JsError::type_error("Custom element has already been constructed")
            })?;
            (Some(id), false)
        }
        Some(Construction::Fresh(entry)) => (entry.take(), true),
        None => (None, true),
    };
    let id = if let Some(id) = existing {
        id
    } else {
        bare(host, definition.name.clone())?
    };
    let element = wrap(vm, host, Some(id))?;
    vm.set_prototype(element.as_object().expect("DOM wrapper"), proto)?;
    if fresh {
        status(host, id, Status::Custom);
    }
    Ok(element)
}

pub(super) fn created(vm: &mut Vm, host: &Shared, initial: NodeId) -> Result<Value, JsError> {
    let definition = definition(&host.borrow(), initial);
    if let Some(definition) = definition {
        definition
            .construction
            .borrow_mut()
            .push(Construction::Fresh(Some(initial)));
        let constructed = vm.construct(definition.constructor.clone(), &[]);
        let frame = definition.construction.borrow_mut().pop();
        let result = constructed.and_then(|value| {
            let id = node(host,&value)?;
            let state = host.borrow();
            let doc = &state.document;
            if doc.namespace(id) != Namespace::Html || doc.tag(id) != definition.name
                || !matches!(&doc.nodes[id.0].kind, NodeKind::Element {attrs, ..} if attrs.is_empty())
                || doc.nodes[id.0].parent.is_some() || !doc.nodes[id.0].children.is_empty()
                || state.custom_elements.borrow().elements.get(&id) != Some(&Status::Custom) {
                return Err(JsError::type_error("Invalid result from custom element constructor"));
            }
            Ok(value)
        });
        match result {
            Ok(value) => return Ok(value),
            Err(error) => report(host, &definition.name, error)?,
        }
        // Author code may have retained or modified the element initialized by
        // super(). Return an untouched, failed element to createElement's caller.
        let fallback = if matches!(frame, Some(Construction::Fresh(Some(_)))) {
            initial
        } else {
            bare(host, definition.name.clone())?
        };
        status(host, fallback, Status::Failed);
        return wrap(vm, host, Some(fallback));
    }
    wrap(vm, host, Some(initial))
}

pub(super) fn attribute(
    state: &State,
    id: NodeId,
    name: &str,
    old: Option<&str>,
    value: Option<&str>,
    namespace: Option<AttributeNamespace>,
) -> Option<Reaction> {
    if old.is_none() && value.is_none()
        || state.custom_elements.borrow().elements.get(&id) != Some(&Status::Custom)
    {
        return None;
    }
    let def = definition(state, id)?;
    attr_reaction(&def, id, name, old, value, namespace)
}

fn attr_reaction(
    def: &Definition,
    id: NodeId,
    name: &str,
    old: Option<&str>,
    value: Option<&str>,
    namespace: Option<AttributeNamespace>,
) -> Option<Reaction> {
    let local = if namespace.is_some() {
        name.rsplit(':').next().unwrap_or(name)
    } else {
        name
    };
    if !def.observed.contains(local) {
        return None;
    }
    Some(Reaction::Call(
        id,
        def.changed.clone()?,
        vec![
            local.into(),
            old.map_or(Value::Null, Into::into),
            value.map_or(Value::Null, Into::into),
            namespace.map_or(Value::Null, |ns| ns.uri().into()),
        ],
    ))
}

fn upgrade(vm: &mut Vm, host: &Shared, id: NodeId) -> Result<(), JsError> {
    let (def, callbacks) = {
        let state = host.borrow();
        if state.custom_elements.borrow().elements.contains_key(&id) {
            return Ok(());
        }
        let Some(def) = definition(&state, id) else {
            return Ok(());
        };
        let doc = &state.document;
        let mut callbacks = Vec::new();
        if let NodeKind::Element { attrs, .. } = &doc.nodes[id.0].kind {
            for (name, val) in attrs {
                if let Some(reaction) = attr_reaction(
                    &def,
                    id,
                    name,
                    None,
                    Some(val),
                    doc.attribute_namespace(id, name),
                ) {
                    callbacks.push(reaction);
                }
            }
        }
        if doc.is_connected(id)
            && let Some(callback) = &def.connected
        {
            callbacks.push(Reaction::Call(id, callback.clone(), vec![]));
        }
        (def, callbacks)
    };
    let wrapper = wrap(vm, host, Some(id))?;
    status(host, id, Status::Building);
    def.construction
        .borrow_mut()
        .push(Construction::Upgrade(Some(id)));
    let constructed = vm.construct(def.constructor.clone(), &[]);
    let consumed = matches!(
        def.construction.borrow_mut().pop(),
        Some(Construction::Upgrade(None))
    );
    let result = constructed.and_then(|value| {
        if consumed && value == wrapper {
            Ok(())
        } else {
            Err(JsError::type_error(
                "Custom element constructor did not initialize its upgrade candidate",
            ))
        }
    });
    if let Err(error) = result {
        status(host, id, Status::Failed);
        return report(host, &def.name, error);
    }
    status(host, id, Status::Custom);
    for callback in callbacks {
        deliver(vm, host, callback)?;
    }
    Ok(())
}

// Cloning/importing into the active document uses its custom element registry.
// Parsed and other inert documents have no active registry. Template contents
// are excluded by the shadow-including traversal; they remain inert.
pub(super) fn upgrade_cloned_tree(
    vm: &mut Vm,
    host: &Shared,
    root: NodeId,
) -> Result<(), JsError> {
    let candidates = {
        let state = host.borrow();
        if state.document.node_document(root) != state.document.root
            || !state.custom_elements.borrow().has_definitions()
        {
            return Ok(());
        }
        let mut candidates = Vec::new();
        for id in state.document.shadow_including_nodes(root) {
            vm.charge_work(1)?;
            if definition(&state, id).is_some() {
                candidates.push(id);
            }
        }
        candidates
    };
    for id in candidates {
        vm.charge_work(1)?;
        upgrade(vm, host, id)?;
    }
    Ok(())
}

pub(super) fn connection(state: &State, parent: NodeId, subtree: NodeId, added: bool) {
    state
        .custom_elements
        .borrow_mut()
        .connection(&state.document, parent, subtree, added);
}

impl Registry {
    pub(super) fn has_definition(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }

    pub(super) fn shadow_disabled(&self, tag: &str) -> bool {
        self.definitions
            .get(tag)
            .is_some_and(|definition| definition.disable_shadow)
    }
    pub(super) fn has_definitions(&self) -> bool {
        !self.definitions.is_empty()
    }
    pub(super) fn pending(&self) -> bool {
        !self.reactions.is_empty()
    }
    pub(super) fn connection(
        &mut self,
        doc: &Document,
        parent: NodeId,
        subtree: NodeId,
        added: bool,
    ) {
        if !self.has_definitions() || !doc.is_connected(parent) {
            return;
        }

        for id in doc.shadow_including_nodes(subtree) {
            if doc.namespace(id) != Namespace::Html {
                continue;
            }
            let Some(def) = self.definitions.get(doc.tag(id)) else {
                continue;
            };
            let reaction = match self.elements.get(&id) {
                None if added => Some(Reaction::Upgrade(id)),
                Some(Status::Custom) => {
                    let callback = if added {
                        &def.connected
                    } else {
                        &def.disconnected
                    };
                    callback
                        .clone()
                        .map(|callback| Reaction::Call(id, callback, vec![]))
                }
                _ => None,
            };
            if let Some(reaction) = reaction {
                self.reactions.push(reaction);
            }
        }
    }
}

pub(super) fn deliver(vm: &mut Vm, host: &Shared, reaction: Reaction) -> Result<(), JsError> {
    vm.charge_work(1)?;
    match reaction {
        Reaction::Upgrade(id) => upgrade(vm, host, id),
        Reaction::Call(id, callback, args) => {
            let element = wrap(vm, host, Some(id))?;
            if let Err(error) = vm.call(callback, element, &args) {
                let name = host.borrow().document.tag(id).to_owned();
                report(host, &name, error)?;
            }
            Ok(())
        }
    }
}

pub(super) fn flush(vm: &mut Vm, host: &Shared) -> Result<(), JsError> {
    // Move out the current queue; a callback may run another DOM operation and
    // must drain that operation's reactions before it returns to the caller.
    let reactions = std::mem::take(&mut host.borrow().custom_elements.borrow_mut().reactions);
    for reaction in reactions {
        deliver(vm, host, reaction)?;
    }
    Ok(())
}

fn callback(vm: &mut Vm, proto: &Value, name: &str) -> Result<Option<Value>, JsError> {
    let value = vm.get_property(proto.clone(), name)?;
    if value == Value::Undefined {
        Ok(None)
    } else if vm.is_callable(&value) {
        Ok(Some(value))
    } else {
        Err(JsError::type_error(format!("Expected a callable {name}")))
    }
}

fn read_definition(
    vm: &mut Vm,
    name: String,
    constructor: Value,
    remaining: usize,
) -> Result<(Definition, usize), JsError> {
    let proto = vm.get_property(constructor.clone(), "prototype")?;
    if proto.as_object().is_none() {
        return Err(JsError::type_error(
            "Custom element prototype must be an object",
        ));
    }
    let connected = callback(vm, &proto, "connectedCallback")?;
    let disconnected = callback(vm, &proto, "disconnectedCallback")?;
    let changed = callback(vm, &proto, "attributeChangedCallback")?;
    let mut observed = HashSet::new();
    let mut bytes = 0usize;
    if changed.is_some() {
        let list = vm.get_property(constructor.clone(), "observedAttributes")?;
        if list != Value::Undefined {
            let iterator = webidl::iterator_method(vm, &list)?;
            webidl::sequence(vm, list, iterator, super::NODE_LIMIT, |vm, input| {
                let value = text(vm, input)?;
                if !observed.contains(&value) {
                    bytes = bytes.saturating_add(value.len()).saturating_add(64);
                    if bytes > remaining {
                        return Err(JsError::new(
                            ErrorKind::HeapLimit,
                            "Observed custom element attributes exceed the DOM byte budget",
                        ));
                    }
                    observed.insert(value);
                }
                Ok(())
            })?;
        }
    }
    let mut disable_shadow = false;
    let disabled = vm.get_property(constructor.clone(), "disabledFeatures")?;
    if !disabled.is_undefined() {
        let iterator = webidl::iterator_method(vm, &disabled)?;
        webidl::sequence(vm, disabled, iterator, super::NODE_LIMIT, |vm, feature| {
            disable_shadow |= text(vm, feature)? == "shadow";
            Ok(())
        })?;
    }
    Ok((
        Definition {
            name,
            constructor,
            connected,
            disconnected,
            changed,
            observed,
            disable_shadow,
            construction: RefCell::default(),
        },
        bytes,
    ))
}

fn store(vm: &mut Vm, receiver: &Value, key: &str, refs: &[Value]) -> Result<(), JsError> {
    let store = vm
        .host_slot(receiver, REFERENCES_SLOT)
        .expect("registry private references");
    let array = vm.new_array(refs);
    vm.define_data_property(store, key, array, true, false, true)
}

fn define(
    vm: &mut Vm,
    host: &Shared,
    receiver: &Value,
    args: &[Value],
    errors: &Errors,
) -> Result<Value, JsError> {
    webidl::required(args, 2, "define")?;
    let name = text(vm, arg(args, 0))?;
    let constructor = arg(args, 1);
    if !vm.is_constructor(&constructor) {
        return Err(JsError::type_error("Expected a custom element constructor"));
    }
    let options = arg(args, 2);
    let extends = if options.is_nullish() {
        Value::Undefined
    } else {
        if options.as_object().is_none() {
            return Err(JsError::type_error("Expected element definition options"));
        }
        vm.get_property(options, "extends")?
    };
    if !valid_name(&name) {
        return Err(errors.exception(vm, "SyntaxError", "Invalid custom element name"));
    }
    let constructor_id = constructor.as_object().expect("constructor object");
    {
        let state = host.borrow();
        let mut registry = state.custom_elements.borrow_mut();
        if registry.defining
            || registry.definitions.contains_key(&name)
            || registry.by_constructor.contains_key(&constructor_id)
        {
            return Err(errors.exception(
                vm,
                "NotSupportedError",
                "A definition is active or the name or constructor is registered",
            ));
        }
        if extends != Value::Undefined {
            return Err(errors.exception(
                vm,
                "NotSupportedError",
                "Customized built-in elements are unsupported",
            ));
        }
        registry.defining = true;
    }
    let remaining = super::DOM_BYTES_LIMIT
        .saturating_sub(host.borrow().custom_elements.borrow().observed_bytes);
    let result = read_definition(vm, name.clone(), constructor.clone(), remaining);
    host.borrow().custom_elements.borrow_mut().defining = false;
    let (def, observed_bytes) = result?;
    let def = Rc::new(def);
    let mut refs = vec![constructor.clone()];
    refs.extend(
        [
            def.connected.clone(),
            def.disconnected.clone(),
            def.changed.clone(),
        ]
        .into_iter()
        .flatten(),
    );
    // The GC traces this private store through the registry, even if the author
    // removes a callback from the prototype after registering the definition.
    let candidates = {
        let state = host.borrow();
        let mut ids = vec![];
        for id in state.document.shadow_including_nodes(state.document.root) {
            vm.charge_work(1)?;
            if state.document.namespace(id) == Namespace::Html && state.document.tag(id) == name {
                ids.push(id);
            }
        }
        ids
    };
    let private_key = format!("definition:{name}");
    store(vm, receiver, &private_key, &refs)?;
    let pending = {
        let state = host.borrow();
        let mut registry = state.custom_elements.borrow_mut();
        registry.definitions.insert(name.clone(), def.clone());
        registry.by_constructor.insert(constructor_id, def);
        registry.observed_bytes += observed_bytes;
        registry.pending.remove(&name)
    };
    if let Some(capability) = pending {
        vm.call(capability.resolve, Value::Undefined, &[constructor])?;
        let refs = vm
            .host_slot(receiver, REFERENCES_SLOT)
            .expect("registry private references");
        vm.define_data_property(
            refs,
            &format!("pending:{name}"),
            Value::Undefined,
            true,
            false,
            true,
        )?;
    }
    for id in candidates {
        upgrade(vm, host, id)?;
    }
    Ok(Value::Undefined)
}

pub(super) fn install(vm: &mut Vm, host: &Shared, errors: &Errors) -> Result<(), JsError> {
    let proto = vm.new_object();
    webidl::interface(
        vm,
        proto.clone(),
        "CustomElementRegistry",
        0,
        |_, _, _, _| {
            Err(JsError::type_error(
                "Illegal CustomElementRegistry constructor",
            ))
        },
    )?;
    let registry = vm.new_object_with_prototype(proto.clone())?;
    let store_object = vm.new_object_with_prototype(Value::Null)?;
    vm.set_host_slot(registry.clone(), REFERENCES_SLOT, store_object)?;
    let window = host.borrow().window.clone();
    vm.set_host_slot(window.clone(), REGISTRY_SLOT, registry.clone())?;
    let exposed = registry.clone();
    super::getter(vm, window, "customElements", move |_, _, _| {
        Ok(exposed.clone())
    })?;
    for (name, arity) in [("get", 1), ("define", 2), ("whenDefined", 1)] {
        let registry = registry.clone();
        let host = host.clone();
        let errors = errors.clone();
        webidl::operation(vm, proto.clone(), name, arity, move |vm, this, args| {
            if this != registry {
                return Err(JsError::type_error(
                    "Illegal CustomElementRegistry receiver",
                ));
            }
            if name == "define" {
                return define(vm, &host, &this, args, &errors);
            }
            if name == "get" {
                webidl::required(args, 1, name)?;
                let key = text(vm, arg(args, 0))?;
                return Ok(host
                    .borrow()
                    .custom_elements
                    .borrow()
                    .definitions
                    .get(&key)
                    .map_or(Value::Undefined, |def| def.constructor.clone()));
            }
            let result = webidl::required(args, 1, name)
                .and_then(|_| text(vm, arg(args, 0)))
                .and_then(|key| {
                    if valid_name(&key) {
                        Ok(key)
                    } else {
                        Err(errors.exception(vm, "SyntaxError", "Invalid custom element name"))
                    }
                });
            let key = match result {
                Ok(key) => key,
                Err(error) => {
                    if fatal(&error) {
                        return Err(error);
                    }
                    let capability = vm.new_promise_capability()?;
                    let exception = match &error.thrown {
                        Some(value) => value.clone(),
                        None => vm.error_value(&error)?,
                    };
                    vm.call(capability.reject, Value::Undefined, &[exception])?;
                    return Ok(capability.promise);
                }
            };
            let constructor = {
                let state = host.borrow();
                let registry = state.custom_elements.borrow();
                if let Some(existing) = registry.pending.get(&key) {
                    return Ok(existing.promise.clone());
                }
                registry
                    .definitions
                    .get(&key)
                    .map(|def| def.constructor.clone())
            };
            let capability = vm.new_promise_capability()?;
            if let Some(constructor) = constructor {
                vm.call(capability.resolve, Value::Undefined, &[constructor])?;
            } else {
                store(
                    vm,
                    &this,
                    &format!("pending:{key}"),
                    &[capability.promise.clone(), capability.resolve.clone()],
                )?;
                host.borrow()
                    .custom_elements
                    .borrow_mut()
                    .pending
                    .insert(key, capability.clone());
            }
            Ok(capability.promise)
        })?;
    }
    Ok(())
}

//! Location and History bindings. Address and history state changes are
//! synchronous. Cross-document navigation and history events are browser tasks.
use super::{
    Shared, abort, arg, dispatch_prepared, document_base, drain_microtask_queue, event_target,
    getter, layout_api::LayoutHost, webidl,
};
use crate::{
    history::{
        HistoryScroll, HistoryTransition, NavigationMode, ScriptNavigation, SessionHistory,
        can_rewrite,
        state_data::{SerializeError, StateData},
    },
    layout::ScrollOffset,
};
use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{collections::VecDeque, rc::Rc};
use url::{Url, quirks};

const ADDRESS_LIMIT: usize = 128 * 1024;
const PENDING_LIMIT: usize = 64;
const STATE_SLOT: u32 = 0;
const POP_STATE: u32 = 2;
const OLD_URL: u32 = 3;
const NEW_URL: u32 = 4;

struct HashChange {
    old: Url,
    new: Url,
}

pub(super) struct Routing {
    pub(super) history: SessionHistory,
    object: Value,
    cached: bool,
    commands: VecDeque<ScriptNavigation>,
    events: VecDeque<HashChange>,
    pop_prototype: Value,
    hash_prototype: Value,
    scroll: Option<HistoryScroll>,
}

impl Routing {
    pub(super) fn new(history: SessionHistory) -> Self {
        Self {
            history,
            object: Value::Undefined,
            cached: false,
            commands: VecDeque::new(),
            events: VecDeque::new(),
            pop_prototype: Value::Undefined,
            hash_prototype: Value::Undefined,
            scroll: None,
        }
    }

    pub(super) fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    pub(super) fn has_commands(&self) -> bool {
        !self.commands.is_empty()
    }

    pub(super) fn next_command(&mut self) -> Option<ScriptNavigation> {
        self.commands.pop_front()
    }

    pub(super) fn take_scroll(&mut self) -> Option<HistoryScroll> {
        self.scroll.take()
    }

    fn submit(&mut self, command: ScriptNavigation) -> Result<(), JsError> {
        if self.commands.len() >= PENDING_LIMIT {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Navigation request queue is full",
            ));
        }
        self.commands.push_back(command);
        Ok(())
    }
}

fn scroll(layout: &LayoutHost) -> ScrollOffset {
    let viewport = layout.viewport();
    ScrollOffset {
        x: viewport.scroll_x,
        y: viewport.scroll_y,
    }
}

fn current_state(vm: &mut Vm, dom: &Shared) -> Result<Value, JsError> {
    let (object, cached, value) = {
        let state = dom.borrow();
        (
            state.routing.object.clone(),
            state.routing.cached,
            state.routing.history.state(),
        )
    };
    if cached {
        return Ok(vm.host_slot(&object, STATE_SLOT).unwrap_or(Value::Null));
    }
    let value = value.map_or(Ok(Value::Null), |data| data.restore(vm))?;
    vm.set_host_slot(object, STATE_SLOT, value.clone())?;
    dom.borrow_mut().routing.cached = true;
    Ok(value)
}

fn publish_address(
    vm: &mut Vm,
    dom: &Shared,
    address: Url,
    changed_entry: bool,
) -> Result<(), JsError> {
    let mut state = dom.borrow_mut();
    if changed_entry {
        vm.set_host_slot(state.routing.object.clone(), STATE_SLOT, Value::Undefined)?;
        state.routing.cached = false;
    }
    if state.url != address {
        document_base(&state);
        state.url = address;
        state.dirty = true;
    }
    Ok(())
}

pub(super) fn apply_transition(
    vm: &mut Vm,
    dom: &Shared,
    transition: HistoryTransition,
) -> Result<(), JsError> {
    publish_address(vm, dom, transition.url.clone(), transition.changed_entry)?;
    dom.borrow_mut().routing.scroll = Some(transition.scroll);
    if transition.changed_entry {
        let prototype = dom.borrow().routing.pop_prototype.clone();
        let value = current_state(vm, dom)?;
        fire_event(vm, dom, prototype, "popstate", &[(POP_STATE, value)])?;
    }
    if transition.previous.fragment() != transition.url.fragment() {
        dom.borrow_mut().routing.events.push_back(HashChange {
            old: transition.previous,
            new: transition.url,
        });
    }
    Ok(())
}

pub(super) fn run_events(vm: &mut Vm, dom: &Shared) -> Result<(), JsError> {
    for _ in 0..32 {
        let Some(HashChange { old, new }) = dom.borrow_mut().routing.events.pop_front() else {
            break;
        };
        let prototype = dom.borrow().routing.hash_prototype.clone();
        fire_event(
            vm,
            dom,
            prototype,
            "hashchange",
            &[
                (OLD_URL, old.as_str().into()),
                (NEW_URL, new.as_str().into()),
            ],
        )?;
        drain_microtask_queue(vm, dom)?;
    }
    Ok(())
}

fn fire_event(
    vm: &mut Vm,
    dom: &Shared,
    prototype: Value,
    name: &str,
    fields: &[(u32, Value)],
) -> Result<(), JsError> {
    let object = vm.new_object_with_prototype(prototype)?;
    let event = event_target::init_event(vm, object, name.into(), [false; 3], true)?;
    for (slot, value) in fields {
        vm.set_host_slot(event.clone(), *slot, value.clone())?;
    }
    dispatch_prepared(vm, dom, None, event, &[])?;
    Ok(())
}

#[derive(Clone)]
struct Host {
    dom: Shared,
    layout: LayoutHost,
    exception: Value,
    is_enumerable: Value,
}

impl Host {
    fn base(&self) -> Url {
        document_base(&self.dom.borrow())
    }

    fn exception(&self, vm: &mut Vm, name: &str, message: &str) -> JsError {
        match abort::dom_exception_error(vm, self.exception.clone(), name, message) {
            Ok(error) | Err(error) => error,
        }
    }

    fn update(&self, vm: &mut Vm, args: &[Value], replace: bool) -> Result<Value, JsError> {
        webidl::required(args, 2, if replace { "replaceState" } else { "pushState" })?;
        vm.to_string(&args[1])?;
        let reference = args
            .get(2)
            .filter(|url| !url.is_nullish())
            .map(|url| webidl::usv_string(vm, url, ADDRESS_LIMIT))
            .transpose()?;
        let serialized = StateData::capture(vm, args[0].clone(), &self.is_enumerable, |id| {
            let state = self.dom.borrow();
            state.window.as_object() != Some(id) && !state.nodes.contains_key(&id)
        })
        .map_err(|error| match error {
            SerializeError::Uncloneable => self.exception(
                vm,
                "DataCloneError",
                "History state contains an uncloneable value",
            ),
            SerializeError::Limit => self.exception(
                vm,
                "QuotaExceededError",
                "History state exceeds its storage limit",
            ),
            SerializeError::JavaScript(error) => error,
        })?;
        let old = self.dom.borrow().url.clone();
        let address = match reference.as_deref().filter(|url| !url.is_empty()) {
            Some(url) => self
                .base()
                .join(url)
                .map_err(|_| self.exception(vm, "SecurityError", "Invalid history URL"))?,
            None => old.clone(),
        };
        if !can_rewrite(&old, &address) {
            return Err(self.exception(
                vm,
                "SecurityError",
                "The history URL cannot replace this document address",
            ));
        }
        let history = self.dom.borrow().routing.history.clone();
        if !history.rewrite(
            address.clone(),
            Rc::new(serialized),
            replace,
            scroll(&self.layout),
        ) {
            return Err(self.exception(
                vm,
                "QuotaExceededError",
                "Session history exceeds its storage limit",
            ));
        }
        publish_address(vm, &self.dom, address, true)?;
        Ok(Value::Undefined)
    }

    fn navigate(&self, vm: &mut Vm, url: Url, mode: NavigationMode) -> Result<Value, JsError> {
        if !matches!(url.scheme(), "http" | "https" | "file" | "about" | "data") {
            return Err(self.exception(
                vm,
                "SyntaxError",
                "Navigation URL scheme is currently unsupported",
            ));
        }
        let (history, queued) = {
            let state = self.dom.borrow();
            (state.routing.history.clone(), state.routing.events.len())
        };
        if queued >= PENDING_LIMIT {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "History event queue is full",
            ));
        }
        if let Some(transition) = history.fragment(url.clone(), mode, scroll(&self.layout)) {
            apply_transition(vm, &self.dom, transition)?;
        } else {
            let mut state = self.dom.borrow_mut();
            let mode = if state.ready_state != "complete" && mode == NavigationMode::Push {
                NavigationMode::Replace
            } else {
                mode
            };
            state
                .routing
                .submit(ScriptNavigation::Navigate { url, mode })?;
        }
        Ok(Value::Undefined)
    }

    fn navigate_reference(
        &self,
        vm: &mut Vm,
        value: &Value,
        mode: NavigationMode,
    ) -> Result<Value, JsError> {
        let value = webidl::usv_string(vm, value, ADDRESS_LIMIT)?;
        let url = self
            .base()
            .join(&value)
            .map_err(|_| self.exception(vm, "SyntaxError", "Invalid navigation URL"))?;
        self.navigate(vm, url, mode)
    }
}

fn brand(receiver: &Value, intrinsic: &Value, interface: &str) -> Result<(), JsError> {
    if receiver != intrinsic {
        return Err(JsError::type_error(format!("Illegal {interface} receiver")));
    }
    Ok(())
}

pub(super) fn install(
    vm: &mut Vm,
    dom: &Shared,
    document: Value,
    layout: &LayoutHost,
) -> Result<(), JsError> {
    let object = vm.get_global("Object")?;
    let proto = vm.get_property(object, "prototype")?;
    let is_enumerable = vm.get_property(proto, "propertyIsEnumerable")?;
    let exception = vm.get_global("DOMException")?;
    vm.root(is_enumerable.clone());
    vm.root(exception.clone());
    let host = Host {
        dom: dom.clone(),
        layout: layout.clone(),
        exception,
        is_enumerable,
    };
    let window = dom.borrow().window.clone();
    let pop_prototype = install_event(vm, "PopStateEvent", &[("state", POP_STATE)])?;
    let hash_prototype = install_event(
        vm,
        "HashChangeEvent",
        &[("oldURL", OLD_URL), ("newURL", NEW_URL)],
    )?;
    {
        let mut state = dom.borrow_mut();
        state.routing.pop_prototype = pop_prototype;
        state.routing.hash_prototype = hash_prototype;
    }
    install_history(vm, &host, window.clone())?;
    install_location(vm, &host, window.clone(), document.clone())?;
    for name in ["URL", "documentURI", "baseURI"] {
        let host = host.clone();
        getter(vm, document.clone(), name, move |vm, _, _| {
            if name == "baseURI" {
                vm.charge_work(host.dom.borrow().document.nodes.len())?;
            }
            let value = if name == "baseURI" {
                host.base()
            } else {
                host.dom.borrow().url.clone()
            };
            Ok(value.as_str().into())
        })?;
    }
    let origin = dom.borrow().url.origin().ascii_serialization();
    getter(vm, window, "origin", move |_, _, _| {
        Ok(origin.as_str().into())
    })
}

fn install_event(
    vm: &mut Vm,
    name: &'static str,
    fields: &'static [(&str, u32)],
) -> Result<Value, JsError> {
    let event = vm.get_global("Event")?;
    let event_proto = vm.get_property(event.clone(), "prototype")?;
    let prototype = vm.new_object_with_prototype(event_proto)?;
    vm.root(prototype.clone());
    let intrinsic = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        name,
        1,
        move |vm, _, args, target| {
            let target =
                target.ok_or_else(|| JsError::type_error(format!("{name} requires new")))?;
            webidl::required(args, 1, name)?;
            let kind = event_target::kind(vm, &args[0])?;
            let options = arg(args, 1);
            let flags = event_target::flags(vm, &options)?;
            let object = webidl::instance(vm, target, &intrinsic)?;
            let object = event_target::init_event(vm, object, kind, flags, false)?;
            for (key, slot) in fields {
                let value = event_target::option(vm, &options, key)?;
                let value = if *slot == POP_STATE {
                    if value.is_undefined() {
                        Value::Null
                    } else {
                        value
                    }
                } else if value.is_undefined() {
                    "".into()
                } else {
                    Value::String(vm.to_string(&value)?)
                };
                vm.set_host_slot(object.clone(), *slot, value)?;
            }
            Ok(object)
        },
    )?;
    let object = vm.get_global("Object")?;
    let inherit = vm.get_property(object, "setPrototypeOf")?;
    vm.call(inherit, Value::Undefined, &[constructor, event])?;
    for &(key, slot) in fields {
        getter(vm, prototype.clone(), key, move |vm, object, _| {
            vm.host_slot(&object, slot)
                .ok_or_else(|| JsError::type_error(format!("Illegal {name} receiver")))
        })?;
    }
    Ok(prototype)
}

fn install_history(vm: &mut Vm, host: &Host, window: Value) -> Result<(), JsError> {
    let proto = vm.new_object();
    webidl::interface(vm, proto.clone(), "History", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal History constructor"))
    })?;
    let object = vm.new_object_with_prototype(proto.clone())?;
    vm.root(object.clone());
    vm.set_host_slot(object.clone(), STATE_SLOT, Value::Undefined)?;
    host.dom.borrow_mut().routing.object = object.clone();
    let history = object.clone();
    getter(vm, window, "history", move |_, _, _| Ok(history.clone()))?;
    for name in ["length", "state", "scrollRestoration"] {
        let intrinsic = object.clone();
        let dom = host.dom.clone();
        let get = vm.new_native_function(name, move |vm, receiver, _| {
            brand(&receiver, &intrinsic, "History")?;
            match name {
                "length" => Ok((dom.borrow().routing.history.length() as u32).into()),
                "state" => current_state(vm, &dom),
                _ => Ok(if dom.borrow().routing.history.auto_scroll() {
                    "auto"
                } else {
                    "manual"
                }
                .into()),
            }
        });
        let set = if name == "scrollRestoration" {
            let dom = host.dom.clone();
            let intrinsic = object.clone();
            Some(vm.new_native_function(name, move |vm, receiver, args| {
                brand(&receiver, &intrinsic, "History")?;
                let value = vm.to_string(&arg(args, 0))?;
                let automatic = match value.to_string_lossy().as_str() {
                    "auto" => true,
                    "manual" => false,
                    _ => return Err(JsError::type_error("Invalid scroll restoration mode")),
                };
                dom.borrow().routing.history.set_auto_scroll(automatic);
                Ok(Value::Undefined)
            }))
        } else {
            None
        };
        vm.define_accessor(proto.clone(), name, Some(get), set)?;
    }
    for name in ["back", "forward", "go", "pushState", "replaceState"] {
        let intrinsic = object.clone();
        let host = host.clone();
        let arity = if name.ends_with("State") { 2 } else { 0 };
        webidl::operation(vm, proto.clone(), name, arity, move |vm, receiver, args| {
            brand(&receiver, &intrinsic, "History")?;
            let delta = match name {
                "pushState" => return host.update(vm, args, false),
                "replaceState" => return host.update(vm, args, true),
                "back" => -1,
                "forward" => 1,
                _ => {
                    let number = vm.to_number(&arg(args, 0))?;
                    number.trunc().rem_euclid(4294967296.0) as u32 as i32
                }
            };
            let command = if delta == 0 {
                ScriptNavigation::Reload
            } else {
                ScriptNavigation::Traverse(delta)
            };
            host.dom.borrow_mut().routing.submit(command)?;
            Ok(Value::Undefined)
        })?;
    }
    Ok(())
}

fn install_location(
    vm: &mut Vm,
    host: &Host,
    window: Value,
    document: Value,
) -> Result<(), JsError> {
    let proto = vm.new_object();
    webidl::interface(vm, proto.clone(), "Location", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Location constructor"))
    })?;
    let object = vm.new_object_with_prototype(proto)?;
    vm.forbid_private_elements(&object)?;
    vm.root(object.clone());
    for receiver in [window, document] {
        let location = object.clone();
        let get = vm.new_native_function("get location", move |_, _, _| Ok(location.clone()));
        let host = host.clone();
        let set = vm.new_native_function("set location", move |vm, _, args| {
            host.navigate_reference(vm, &arg(args, 0), NavigationMode::Push)
        });
        unforgeable_accessor(vm, receiver, "location", get, Some(set))?;
    }
    for name in [
        "href", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash",
    ] {
        let dom = host.dom.clone();
        let intrinsic = object.clone();
        let get = vm.new_native_function(name, move |_, receiver, _| {
            brand(&receiver, &intrinsic, "Location")?;
            let state = dom.borrow();
            let url = &state.url;
            let value = match name {
                "href" => quirks::href(url),
                "origin" => return Ok(url.origin().ascii_serialization().into()),
                "protocol" => quirks::protocol(url),
                "host" => quirks::host(url),
                "hostname" => quirks::hostname(url),
                "port" => quirks::port(url),
                "pathname" => quirks::pathname(url),
                "search" => quirks::search(url),
                _ => quirks::hash(url),
            };
            Ok(value.into())
        });
        let intrinsic = object.clone();
        let host = host.clone();
        let set = (name != "origin").then(|| {
            vm.new_native_function(name, move |vm, receiver, args| {
                brand(&receiver, &intrinsic, "Location")?;
                let value = arg(args, 0);
                if name == "href" {
                    return host.navigate_reference(vm, &value, NavigationMode::Push);
                }
                let value = webidl::usv_string(vm, &value, ADDRESS_LIMIT)?;
                let original = host.dom.borrow().url.clone();
                let mut url = original.clone();
                match name {
                    "protocol" => {
                        if quirks::set_protocol(&mut url, &value).is_err() {
                            return Err(host.exception(vm, "SyntaxError", "Invalid URL protocol"));
                        }
                        if !matches!(url.scheme(), "http" | "https") {
                            return Ok(Value::Undefined);
                        }
                    }
                    "host" => {
                        let _ = quirks::set_host(&mut url, &value);
                    }
                    "hostname" => {
                        let _ = quirks::set_hostname(&mut url, &value);
                    }
                    "port" => {
                        let _ = quirks::set_port(&mut url, &value);
                    }
                    "pathname" => quirks::set_pathname(&mut url, &value),
                    "search" => quirks::set_search(&mut url, &value),
                    "hash" => {
                        url.set_fragment(Some(value.strip_prefix('#').unwrap_or(&value)));
                        if url.fragment().unwrap_or_default()
                            == original.fragment().unwrap_or_default()
                        {
                            return Ok(Value::Undefined);
                        }
                    }
                    _ => unreachable!(),
                }
                if name != "hash" && url == original {
                    return Ok(Value::Undefined);
                }
                host.navigate(vm, url, NavigationMode::Push)
            })
        });
        unforgeable_accessor(vm, object.clone(), name, get, set)?;
    }
    for name in ["assign", "replace", "reload", "toString"] {
        let host = host.clone();
        let intrinsic = object.clone();
        let function = vm.new_native_function(name, move |vm, receiver, args| {
            brand(&receiver, &intrinsic, "Location")?;
            match name {
                "toString" => Ok(host.dom.borrow().url.as_str().into()),
                "reload" => {
                    host.dom
                        .borrow_mut()
                        .routing
                        .submit(ScriptNavigation::Reload)?;
                    Ok(Value::Undefined)
                }
                _ => {
                    webidl::required(args, 1, name)?;
                    let mode = if name == "assign" {
                        NavigationMode::Push
                    } else {
                        NavigationMode::Replace
                    };
                    host.navigate_reference(vm, &args[0], mode)
                }
            }
        });
        let arity = u32::from(matches!(name, "assign" | "replace"));
        vm.define_data_property(function.clone(), "length", arity.into(), false, false, true)?;
        vm.define_data_property(object.clone(), name, function, false, true, false)?;
    }
    Ok(())
}

fn unforgeable_accessor(
    vm: &mut Vm,
    receiver: Value,
    name: &str,
    get: Value,
    set: Option<Value>,
) -> Result<(), JsError> {
    let object = vm.get_global("Object")?;
    let define = vm.get_property(object, "defineProperty")?;
    let descriptor = vm.new_object_with_prototype(Value::Null)?;
    vm.set_property(descriptor.clone(), "get", get)?;
    vm.set_property(descriptor.clone(), "set", set.unwrap_or(Value::Undefined))?;
    vm.set_property(descriptor.clone(), "enumerable", true.into())?;
    vm.call(
        define,
        Value::Undefined,
        &[receiver, name.into(), descriptor],
    )?;
    Ok(())
}

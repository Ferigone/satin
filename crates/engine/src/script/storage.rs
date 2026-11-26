//! Web IDL wrappers for the tab and profile storage areas.
use super::{Shared, abort, arg, dispatch_prepared, event_target, getter, webidl};
use crate::web_storage::{Connection, Inbox, StorageFailure, TabStorage, Update};
use rust_browser_javascript::{HostNamedProperties, JsError, JsString, RootId, Value, Vm};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};
use url::Url;

const FIELDS: [(&str, u32); 5] = [
    ("key", 2),
    ("oldValue", 3),
    ("newValue", 4),
    ("url", 5),
    ("storageArea", 6),
];

struct Binding {
    connection: Connection,
    vm_object: Cell<Option<RootId>>,
    prototype: RootId,
    exception: RootId,
    state: Weak<RefCell<super::State>>,
}

fn storage_error(vm: &mut Vm, exception: RootId, failure: StorageFailure) -> JsError {
    let (name, message) = match failure {
        StorageFailure::Quota => (
            "QuotaExceededError",
            "The storage quota has been exceeded".into(),
        ),
        StorageFailure::Unavailable(message) => ("SecurityError", message),
    };
    let class = vm.root_value(exception).unwrap_or_default();
    abort::dom_exception_error(vm, class, name, &message).unwrap_or_else(|error| error)
}

fn receiver(vm: &Vm, value: &Value) -> Result<Rc<Binding>, JsError> {
    vm.host_data::<Binding>(value)
        .ok_or_else(|| JsError::type_error("Illegal Storage receiver"))
}

fn dom_string(vm: &mut Vm, value: &Value) -> Result<JsString, JsError> {
    let string = vm.to_string(value)?;
    vm.charge_work(string.len())?;
    Ok(string)
}

fn nullable_string(vm: &mut Vm, value: &Value) -> Result<Value, JsError> {
    if value.is_nullish() {
        return Ok(Value::Null);
    }
    Ok(dom_string(vm, value)?.into())
}

impl Binding {
    fn object(self: &Rc<Self>, vm: &mut Vm) -> Result<Value, JsError> {
        if let Some(root) = self.vm_object.get() {
            return Ok(vm.root_value(root).unwrap());
        }
        self.connection
            .ensure_loaded()
            .map_err(|failure| storage_error(vm, self.exception, failure))?;
        let wrapper = vm.new_object_with_prototype(vm.root_value(self.prototype).unwrap())?;
        vm.set_host_data(wrapper.clone(), self.clone())?;
        vm.set_host_named_properties(wrapper.clone(), self.clone())?;
        self.vm_object.set(Some(vm.root(wrapper.clone())));
        Ok(wrapper)
    }

    fn update(&self, vm: &mut Vm, update: Update) -> Result<(), JsError> {
        let url = self
            .state
            .upgrade()
            .map_or_else(String::new, |state| state.borrow().url.to_string());
        self.connection
            .update(update, url)
            .map_err(|failure| storage_error(vm, self.exception, failure))
    }
}

impl HostNamedProperties for Binding {
    fn names(&self) -> Vec<JsString> {
        self.connection.keys()
    }

    fn get(&self, key: &JsString) -> Option<Value> {
        self.connection.get(key).map(Value::String)
    }

    fn set(&self, vm: &mut Vm, key: JsString, value: Value) -> Result<(), JsError> {
        let value = dom_string(vm, &value)?;
        self.update(vm, Update::Set(key, value))
    }

    fn delete(&self, vm: &mut Vm, key: &JsString) -> Result<(), JsError> {
        self.update(vm, Update::Remove(key.clone()))
    }
}

fn install_area_prototype(vm: &mut Vm, prototype: &Value) -> Result<(), JsError> {
    getter(vm, prototype.clone(), "length", |vm, object, _| {
        Ok(receiver(vm, &object)?.connection.len().into())
    })?;
    webidl::operation(vm, prototype.clone(), "key", 1, |vm, object, args| {
        let storage = receiver(vm, &object)?;
        webidl::required(args, 1, "Storage.key")?;
        let index = vm.to_number(&args[0])?;
        let index = if index.is_finite() {
            index.trunc().rem_euclid(4_294_967_296.) as u32
        } else {
            0
        };
        Ok(storage
            .connection
            .key(index as usize)
            .map_or(Value::Null, Value::String))
    })?;
    webidl::operation(vm, prototype.clone(), "getItem", 1, |vm, object, args| {
        let storage = receiver(vm, &object)?;
        webidl::required(args, 1, "Storage.getItem")?;
        let key = dom_string(vm, &args[0])?;
        Ok(storage
            .connection
            .get(&key)
            .map_or(Value::Null, Value::String))
    })?;
    webidl::operation(vm, prototype.clone(), "setItem", 2, |vm, object, args| {
        let storage = receiver(vm, &object)?;
        webidl::required(args, 2, "Storage.setItem")?;
        let key = dom_string(vm, &args[0])?;
        storage.set(vm, key, args[1].clone())?;
        Ok(Value::Undefined)
    })?;
    webidl::operation(
        vm,
        prototype.clone(),
        "removeItem",
        1,
        |vm, object, args| {
            let storage = receiver(vm, &object)?;
            webidl::required(args, 1, "Storage.removeItem")?;
            let key = dom_string(vm, &args[0])?;
            storage.delete(vm, &key)?;
            Ok(Value::Undefined)
        },
    )?;
    webidl::operation(vm, prototype.clone(), "clear", 0, |vm, object, _| {
        receiver(vm, &object)?.update(vm, Update::Clear)?;
        Ok(Value::Undefined)
    })?;
    Ok(())
}

fn populated_event(
    vm: &mut Vm,
    object: Value,
    kind: JsString,
    flags: [bool; 3],
    values: [Value; 5],
    trusted: bool,
) -> Result<Value, JsError> {
    let event = event_target::init_event(vm, object, kind, flags, trusted)?;
    for ((_, slot), value) in FIELDS.into_iter().zip(values) {
        vm.set_host_slot(event.clone(), slot, value)?;
    }
    Ok(event)
}

fn storage_area(vm: &Vm, value: Value) -> Result<Value, JsError> {
    if value.is_nullish() {
        return Ok(Value::Null);
    }
    receiver(vm, &value)?;
    Ok(value)
}

fn event_url(vm: &mut Vm, value: &Value) -> Result<Value, JsError> {
    if value.is_undefined() {
        return Ok("".into());
    }
    Ok(webidl::usv_string(vm, value, 128 * 1024)?.into())
}

fn init_dict(vm: &mut Vm, options: &Value) -> Result<[Value; 5], JsError> {
    let key = event_target::option(vm, options, "key")?;
    let key = nullable_string(vm, &key)?;
    let new = event_target::option(vm, options, "newValue")?;
    let new = nullable_string(vm, &new)?;
    let old = event_target::option(vm, options, "oldValue")?;
    let old = nullable_string(vm, &old)?;
    let area = event_target::option(vm, options, "storageArea")?;
    let area = storage_area(vm, area)?;
    let url = event_target::option(vm, options, "url")?;
    let url = event_url(vm, &url)?;
    Ok([key, old, new, url, area])
}

fn install_event(vm: &mut Vm) -> Result<RootId, JsError> {
    let base = vm.get_global("Event")?;
    let prototype = vm.get_property(base.clone(), "prototype")?;
    let prototype = vm.new_object_with_prototype(prototype)?;
    let intrinsic = prototype.clone();
    let constructor = webidl::interface(
        vm,
        prototype.clone(),
        "StorageEvent",
        1,
        move |vm, _, args, new| {
            let new = new.ok_or_else(|| JsError::type_error("StorageEvent requires new"))?;
            webidl::required(args, 1, "StorageEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            let options = arg(args, 1);
            if !options.is_nullish() && options.as_object().is_none() {
                return Err(JsError::type_error(
                    "StorageEvent options must be a dictionary",
                ));
            }
            let flags = event_target::flags(vm, &options)?;
            let values = init_dict(vm, &options)?;
            let object = webidl::instance(vm, new, &intrinsic)?;
            populated_event(vm, object, kind, flags, values, false)
        },
    )?;
    let object = vm.get_global("Object")?;
    let set_prototype = vm.get_property(object, "setPrototypeOf")?;
    vm.call(set_prototype, Value::Undefined, &[constructor, base])?;
    for (name, slot) in FIELDS {
        getter(vm, prototype.clone(), name, move |vm, this, _| {
            vm.host_slot(&this, slot)
                .ok_or_else(|| JsError::type_error("Illegal StorageEvent receiver"))
        })?;
    }
    webidl::operation(
        vm,
        prototype.clone(),
        "initStorageEvent",
        1,
        |vm, this, args| {
            let event = event_target::event(vm, &this)?;
            if vm.host_slot(&this, FIELDS[0].1).is_none() {
                return Err(JsError::type_error("Illegal StorageEvent receiver"));
            }
            webidl::required(args, 1, "StorageEvent.initStorageEvent")?;
            let kind = event_target::kind(vm, &args[0])?;
            let flags = [arg(args, 1).is_truthy(), arg(args, 2).is_truthy(), false];
            let values = [
                nullable_string(vm, &arg(args, 3))?,
                nullable_string(vm, &arg(args, 4))?,
                nullable_string(vm, &arg(args, 5))?,
                event_url(vm, &arg(args, 6))?,
                storage_area(vm, arg(args, 7))?,
            ];
            if !event.active.get() {
                vm.set_host_slot(this.clone(), event_target::EVENT_TARGET_SLOT, Value::Null)?;
                populated_event(vm, this, kind, flags, values, false)?;
            }
            Ok(Value::Undefined)
        },
    )?;
    Ok(vm.root(prototype))
}

pub(super) struct StorageHost {
    event: RootId,
    areas: [Option<Rc<Binding>>; 2],
    inbox: Rc<Inbox>,
}

impl StorageHost {
    pub(super) fn install(
        vm: &mut Vm,
        state: &Shared,
        tab: &TabStorage,
        url: &Url,
    ) -> Result<Self, JsError> {
        let prototype = vm.new_object();
        webidl::interface(vm, prototype.clone(), "Storage", 0, |_, _, _, _| {
            Err(JsError::type_error("Illegal Storage constructor"))
        })?;
        install_area_prototype(vm, &prototype)?;
        let prototype = vm.root(prototype);
        let exception = vm.get_global("DOMException")?;
        let exception = vm.root(exception);
        let origin = crate::web_storage::storage_origin(url);
        let inbox = Rc::new(Inbox::default());
        let areas = [false, true].map(|session| {
            origin.as_ref().map(|origin| {
                Rc::new(Binding {
                    connection: tab.connect(origin, session, &inbox),
                    vm_object: Cell::new(None),
                    prototype,
                    exception,
                    state: Rc::downgrade(state),
                })
            })
        });
        for (name, area) in ["localStorage", "sessionStorage"].into_iter().zip(&areas) {
            let binding = area.clone();
            getter(vm, vm.global(), name, move |vm, this, _| {
                if this != vm.global() {
                    return Err(JsError::type_error("Illegal Window storage receiver"));
                }
                let binding = binding.as_ref().ok_or_else(|| {
                    storage_error(
                        vm,
                        exception,
                        StorageFailure::Unavailable(
                            "Storage requires a non-opaque web origin".into(),
                        ),
                    )
                })?;
                binding.object(vm)
            })?;
        }
        let event = install_event(vm)?;
        Ok(Self {
            event,
            areas,
            inbox,
        })
    }

    pub(super) fn has_events(&self) -> bool {
        !self.inbox.borrow().is_empty()
    }

    pub(super) fn dispatch_next(&self, vm: &mut Vm, state: &Shared) -> Result<bool, JsError> {
        let Some(notice) = self.inbox.borrow_mut().pop_front() else {
            return Ok(false);
        };
        let binding = self.areas[usize::from(notice.session)].as_ref().unwrap();
        let area = binding.object(vm)?;
        let object = vm.new_object_with_prototype(vm.root_value(self.event).unwrap())?;
        let values = [
            notice.key.clone().map_or(Value::Null, Value::String),
            notice.old_value.clone().map_or(Value::Null, Value::String),
            notice.new_value.clone().map_or(Value::Null, Value::String),
            notice.url.as_str().into(),
            area,
        ];
        let event = populated_event(vm, object, "storage".into(), [false; 3], values, true)?;
        dispatch_prepared(vm, state, None, event, &[])?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{loader::LoadedPage, script::ScriptRuntime, web_storage::StorageProfile};

    #[test]
    fn home_shortcut_storage_is_private_to_the_two_exact_builtin_pages() {
        let profile = StorageProfile::default();
        let open = |url| {
            ScriptRuntime::with_storage(
                &LoadedPage::from_html(url, "<!doctype html><title>Storage</title>"),
                &TabStorage::new(profile.clone()),
            )
            .unwrap()
        };
        let mut home = open("about:home");
        home.eval("localStorage.setItem('satin.home.shortcuts', 'home only')")
            .unwrap();
        drop(home);

        let mut web = open("https://example.test/");
        assert_eq!(
            web.eval("localStorage.getItem('satin.home.shortcuts') === null")
                .unwrap(),
            Value::Bool(true)
        );
        web.eval("localStorage.setItem('satin.home.shortcuts', 'web only')")
            .unwrap();
        assert_eq!(
            open("about:home")
                .eval("localStorage.getItem('satin.home.shortcuts') === 'home only'")
                .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            open(crate::loader::START_SHORTCUTS_URL)
                .eval("localStorage.getItem('satin.home.shortcuts') === 'home only'")
                .unwrap(),
            Value::Bool(true)
        );

        for url in [
            "about:version",
            "about:home?other",
            "about:home#other",
            "about:start-shortcuts?other",
            "about:start-shortcuts#other",
            "data:text/html,opaque",
        ] {
            assert_eq!(
                open(url)
                    .eval("(function() { try { void localStorage; } catch (error) { return error.name === 'SecurityError'; } return false; })()")
                    .unwrap(),
                Value::Bool(true),
                "{url}"
            );
        }
    }
}

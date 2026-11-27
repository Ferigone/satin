//! Browser identity and capabilities shared by windows and workers.
use super::{getter, webidl};
use crate::loader::USER_AGENT;
use rust_browser_javascript::{JsError, Value, Vm};

const UI_LANGUAGE: &str = "pl";

pub(super) fn install(vm: &mut Vm, window: &Value) -> Result<(), JsError> {
    let navigator = create(vm, false)?;
    for name in ["navigator", "clientInformation"] {
        let navigator = navigator.clone();
        getter(vm, window.clone(), name, move |_, _, _| {
            Ok(navigator.clone())
        })?;
    }
    Ok(())
}

pub(super) fn install_worker(vm: &mut Vm, scope: &Value) -> Result<(), JsError> {
    let navigator = create(vm, true)?;
    getter(vm, scope.clone(), "navigator", move |vm, this, _| {
        if this != vm.global() {
            return Err(JsError::type_error("Illegal WorkerGlobalScope receiver"));
        }
        Ok(navigator.clone())
    })
}

fn create(vm: &mut Vm, worker: bool) -> Result<Value, JsError> {
    let name = if worker { "WorkerNavigator" } else { "Navigator" };
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), name, 0, move |_, _, _, _| {
        Err(JsError::type_error(format!("Illegal {name} constructor")))
    })?;
    let navigator = vm.new_object_with_prototype(prototype.clone())?;
    vm.mark_platform_object(&navigator)?;
    vm.root(navigator.clone());

    let languages = vm.try_new_array(&[UI_LANGUAGE.into()])?;
    let object = vm.get_global("Object")?;
    let freeze = vm.get_property(object.clone(), "freeze")?;
    vm.call(freeze, object, &[languages.clone()])?;
    vm.root(languages.clone());

    let platform = if cfg!(windows) {
        "Win32".to_owned()
    } else {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    };
    let app_version = USER_AGENT.strip_prefix("Mozilla/").unwrap_or(USER_AGENT);
    let concurrency = std::thread::available_parallelism().map_or(1, |count| count.get() as u32);
    let shared = [
        ("appCodeName", "Mozilla".into()),
        ("appName", "Netscape".into()),
        ("appVersion", app_version.into()),
        ("platform", platform.into()),
        ("product", "Gecko".into()),
        ("userAgent", USER_AGENT.into()),
        ("language", UI_LANGUAGE.into()),
        ("languages", languages),
        ("hardwareConcurrency", concurrency.into()),
        // The transport attempts network requests. An unknown link is potentially online.
        ("onLine", Value::Bool(true)),
    ];
    let window_only = [
        ("vendor", "".into()),
        ("vendorSub", "".into()),
        ("cookieEnabled", Value::Bool(true)),
    ];
    for (property, value) in shared.into_iter().chain(window_only.into_iter().filter(|_| !worker)) {
        let expected = navigator.clone();
        getter(vm, prototype.clone(), property, move |_, this, _| {
            if this != expected {
                return Err(JsError::type_error(format!("Illegal {name} receiver")));
            }
            Ok(value.clone())
        })?;
    }
    Ok(navigator)
}

//! Window identity and capabilities provided by this browser.
use super::{getter, webidl};
use crate::loader::USER_AGENT;
use rust_browser_javascript::{JsError, Value, Vm};

const UI_LANGUAGE: &str = "pl";

pub(super) fn install(vm: &mut Vm, window: &Value) -> Result<(), JsError> {
    let prototype = vm.new_object();
    webidl::interface(vm, prototype.clone(), "Navigator", 0, |_, _, _, _| {
        Err(JsError::type_error("Illegal Navigator constructor"))
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
    for (name, value) in [
        ("appCodeName", "Mozilla".into()),
        ("appName", "Netscape".into()),
        ("appVersion", app_version.into()),
        ("platform", platform.into()),
        ("product", "Gecko".into()),
        ("userAgent", USER_AGENT.into()),
        ("vendor", "".into()),
        ("vendorSub", "".into()),
        ("language", UI_LANGUAGE.into()),
        ("languages", languages),
        ("cookieEnabled", Value::Bool(true)),
        ("hardwareConcurrency", concurrency.into()),
    ] {
        let expected = navigator.clone();
        getter(vm, prototype.clone(), name, move |_, this, _| {
            if this != expected {
                return Err(JsError::type_error("Illegal Navigator receiver"));
            }
            Ok(value.clone())
        })?;
    }
    for name in ["navigator", "clientInformation"] {
        let navigator = navigator.clone();
        getter(vm, window.clone(), name, move |_, _, _| {
            Ok(navigator.clone())
        })?;
    }
    Ok(())
}

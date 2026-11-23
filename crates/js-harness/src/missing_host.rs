//! Missing required Test262 host facilities. Reading one stops the run with
//! a diagnostic; it does not return a value that can satisfy an assertion.

use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};

const FACILITIES: [(&str, &str); 4] = [
    ("createRealm", "$262.createRealm"),
    ("agent", "$262.agent"),
    ("gc", "$262.gc"),
    ("AbstractModuleSource", "$262.AbstractModuleSource"),
];

pub(super) fn install(vm: &mut Vm, host: &Value) -> Result<(), JsError> {
    for (property, facility) in FACILITIES {
        let getter = vm.new_native_function(property, move |_, _, _| {
            Err(JsError::new(ErrorKind::UnsupportedFeature, facility))
        });
        vm.define_accessor(host.clone(), property, Some(getter), None)?;
    }
    Ok(())
}

pub(super) fn facility(error: &JsError) -> Option<&'static str> {
    if error.kind != ErrorKind::UnsupportedFeature {
        return None;
    }
    FACILITIES
        .iter()
        .map(|&(_, facility)| facility)
        .find(|&facility| error.message == facility)
}

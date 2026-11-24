//! Missing required Test262 host facilities. Reading one stops the run with
//! a diagnostic; it does not return a value that can satisfy an assertion.

use rust_browser_javascript::{ErrorKind, JsError, Value, Vm};
use std::{sync::OnceLock, time::Instant};

const FACILITIES: [(&str, &str); 3] = [
    ("agent", "$262.agent"),
    ("gc", "$262.gc"),
    ("AbstractModuleSource", "$262.AbstractModuleSource"),
];

pub(super) fn install(vm: &mut Vm, host: &Value) -> Result<(), JsError> {
    let agent = vm.new_object();
    let monotonic_now = vm.new_native_function("monotonicNow", |_, _, _| {
        static START: OnceLock<Instant> = OnceLock::new();
        Ok((START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0).into())
    });
    vm.define_data_property(
        agent.clone(),
        "monotonicNow",
        monotonic_now,
        true,
        true,
        true,
    )?;
    // The upstream helper binds this function while setting up single-agent
    // operations. Actually receiving a report requires the multi-agent host.
    let get_report = vm.new_native_function("getReport", |_, _, _| {
        Err(JsError::new(ErrorKind::UnsupportedFeature, "$262.agent"))
    });
    vm.define_data_property(agent.clone(), "getReport", get_report, true, true, true)?;
    for name in [
        "start",
        "broadcast",
        "receiveBroadcast",
        "report",
        "leaving",
        "sleep",
    ] {
        let get = vm.new_native_function(name, |_, _, _| {
            Err(JsError::new(ErrorKind::UnsupportedFeature, "$262.agent"))
        });
        vm.define_accessor(agent.clone(), name, Some(get), None)?;
    }
    vm.define_data_property(host.clone(), "agent", agent, true, true, true)?;
    for (property, facility) in FACILITIES.into_iter().filter(|(name, _)| *name != "agent") {
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

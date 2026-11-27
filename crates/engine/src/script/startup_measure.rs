use super::*;
use ring::digest::{SHA256, digest};
use std::sync::{OnceLock, atomic::{AtomicUsize, Ordering}};

pub(super) fn evaluate(
    vm: &mut Vm,
    source: &str,
    identifier: &str,
    node: NodeId,
    external: bool,
) -> Result<Value, JsError> {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("SATIN_QA_DISABLE_SCRIPT_PROFILE").is_some()) {
        return vm.eval_with_identifier(source, identifier);
    }
    let (result, measurement) = rust_browser_javascript::startup_profile::measure(|| {
        vm.eval_with_identifier(source, identifier)
    });
    static REPORTS: AtomicUsize = AtomicUsize::new(0);
    if let (Err(error), Some(m)) = (&result, measurement)
        && REPORTS
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < 128).then_some(n + 1)
            })
            .is_ok()
    {
        let hash: String = digest(&SHA256, source.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        eprintln!(
            "SATIN_QA_SCRIPT {}",
            serde_json::json!({
                "node": node.0, "external": external, "sha256": hash, "bytes": source.len(),
                "error": format!("{:?}", error.kind), "wallUs": m.wall_us, "cpuUs": m.cpu_us,
                "cycles": m.cycles, "work": m.work,
            })
        );
        eprintln!("SATIN_QA_FUNCTIONS {}", serde_json::json!({
            "sha256": hash, "top": m.top_functions, "stack": m.denied_stack,
            "unassignedWork": m.unassigned_work,
        }));
    }
    result
}

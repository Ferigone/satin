//! Match the runner's canonical fixture identifiers to the VM's static requests.
use rust_browser_javascript::{ErrorKind, JsError, JsString, ModuleId, Vm};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModuleFile {
    pub identifier: String,
    pub source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModuleEdge {
    pub referrer: String,
    pub specifier: Vec<u16>,
    pub target: String,
}

pub(super) struct LinkFailure {
    pub phase: &'static str,
    pub error: JsError,
}
impl LinkFailure {
    fn host(message: &str) -> Self {
        Self {
            phase: "host",
            error: JsError::new(ErrorKind::InternalError, message),
        }
    }
}

pub(super) fn link(
    vm: &mut Vm,
    root: ModuleId,
    identifier: &str,
    files: Vec<ModuleFile>,
    edges: Vec<ModuleEdge>,
) -> Result<BTreeMap<String, ModuleId>, LinkFailure> {
    if files.len() > 1023 || edges.len() > 16384 {
        return Err(LinkFailure {
            phase: "resolution",
            error: JsError::new(
                ErrorKind::ExecutionLimit,
                "Test262 module fixture graph budget exhausted",
            ),
        });
    }
    let mut sources = BTreeMap::new();
    for file in files {
        if sources.insert(file.identifier, file.source).is_some() {
            return Err(LinkFailure::host("Duplicate module fixture identifier"));
        }
    }
    let mut resolutions = BTreeMap::new();
    for edge in edges {
        let key = (
            JsString::from(edge.referrer),
            JsString::from_utf16(edge.specifier),
        );
        if resolutions.insert(key, edge.target).is_some() {
            return Err(LinkFailure::host("Duplicate module resolution edge"));
        }
    }
    let mut modules = BTreeMap::from([(identifier.to_owned(), root)]);
    let mut phase = "resolution";
    let result = vm.link_module(root, |vm, referrer, specifier| {
        let referrer = vm.module_identifier(referrer)?;
        let target = resolutions
            .get(&(referrer, specifier.clone()))
            .ok_or_else(|| {
                phase = "host";
                JsError::new(
                    ErrorKind::InternalError,
                    "Test262 module fixture could not be resolved",
                )
            })?;
        if let Some(module) = modules.get(target) {
            return Ok(*module);
        }
        let source = sources.get(target).ok_or_else(|| {
            phase = "host";
            JsError::new(
                ErrorKind::InternalError,
                "Test262 module fixture is absent from the pinned tree",
            )
        })?;
        let module = vm.parse_module(source, target)?;
        modules.insert(target.clone(), module);
        Ok(module)
    });
    result
        .map(|()| modules)
        .map_err(|error| LinkFailure { phase, error })
}

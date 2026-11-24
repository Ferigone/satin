//! Match the runner's canonical fixture identifiers to the VM's static requests.
use rust_browser_javascript::{ErrorKind, JsError, JsString, ModuleId, ModuleType, Vm};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModuleFile {
    pub identifier: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub bytes: Option<Vec<u8>>,
}

impl ModuleFile {
    pub(super) fn byte_len(&self) -> usize {
        self.bytes.as_ref().map_or(self.source.len(), Vec::len)
    }
    pub(super) fn into_parts(self) -> (String, Vec<u8>) {
        (
            self.identifier,
            self.bytes.unwrap_or_else(|| self.source.into_bytes()),
        )
    }
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
    root_source: &str,
    files: Vec<ModuleFile>,
    edges: Vec<ModuleEdge>,
) -> Result<BTreeMap<(String, ModuleType), ModuleId>, LinkFailure> {
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
        let (identifier, bytes) = file.into_parts();
        if sources.insert(identifier, bytes).is_some() {
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
    let mut modules = BTreeMap::from([((identifier.to_owned(), ModuleType::JavaScript), root)]);
    let mut phase = "resolution";
    let result = vm.link_module_with_requests(root, |vm, referrer, request| {
        let module_type = request
            .module_type()
            .map_err(|e| JsError::new(ErrorKind::SyntaxError, e.message))?;
        let referrer = vm.module_identifier(referrer)?;
        let target = resolutions
            .get(&(referrer, request.specifier.clone()))
            .ok_or_else(|| {
                phase = "host";
                JsError::new(
                    ErrorKind::InternalError,
                    "Test262 module fixture could not be resolved",
                )
            })?;
        let cache_key = (target.clone(), module_type);
        if let Some(module) = modules.get(&cache_key) {
            return Ok(*module);
        }
        let source = if target == identifier {
            Some(root_source.as_bytes())
        } else {
            sources.get(target).map(Vec::as_slice)
        }
        .ok_or_else(|| {
            phase = "host";
            JsError::new(
                ErrorKind::InternalError,
                "Test262 module fixture is absent from the pinned tree",
            )
        })?;
        let module = parse_fixture(vm, source, target, module_type)?;
        modules.insert(cache_key, module);
        Ok(module)
    });
    result
        .map(|()| modules)
        .map_err(|error| LinkFailure { phase, error })
}

pub(super) fn parse_fixture(
    vm: &mut Vm,
    source: &[u8],
    name: &str,
    kind: ModuleType,
) -> Result<ModuleId, JsError> {
    let path = name.rsplit_once('#').map_or(name, |(path, _)| path);
    if (kind == ModuleType::Json && !path.ends_with(".json"))
        || (kind == ModuleType::JavaScript && path.ends_with(".json"))
    {
        return Err(JsError::type_error(
            "Fixture MIME type does not match the requested module type",
        ));
    }
    vm.parse_module_bytes(source, name, kind)
}

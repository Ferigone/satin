//! Runtime loader for source-text fixtures from pinned Test262 blobs.
use crate::module_host::ModuleFile;
use rust_browser_javascript::{
    DynamicImportRequest, ErrorKind, JsError, JsString, ModuleId, ModuleType, RealmId, RootId, Value, Vm,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};

struct Queued {
    realm: RealmId,
    request: DynamicImportRequest,
    roots: [RootId; 3],
}

pub(super) struct DynamicHost {
    source_name: String,
    sources: BTreeMap<String, Vec<u8>>,
    modules: BTreeMap<(RealmId, String, ModuleType), ModuleId>,
    queued: Rc<RefCell<VecDeque<Queued>>>,
}

impl DynamicHost {
    pub(super) fn install(
        vm: &mut Vm,
        identifier: &str,
        original: String,
        files: Vec<ModuleFile>,
        modules: BTreeMap<(String, ModuleType), ModuleId>,
    ) -> Result<Self, JsError> {
        if files.len() > 1023
            || files.iter().map(ModuleFile::byte_len).sum::<usize>() > 4 * 1024 * 1024
        {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Dynamic fixture budget exhausted",
            ));
        }
        let source_name = fixture_identifier(identifier).to_owned();
        let mut sources = BTreeMap::from([(source_name.clone(), original.into_bytes())]);
        for file in files {
            let (identifier, bytes) = file.into_parts();
            if sources.insert(identifier, bytes).is_some() {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Duplicate dynamic fixture",
                ));
            }
        }
        let queued = Rc::new(RefCell::new(VecDeque::new()));
        let incoming = queued.clone();
        let max = vm.limits().max_pending_jobs.min(256);
        vm.set_inherited_dynamic_import_hook(move |vm, request| {
            let mut queue = incoming.borrow_mut();
            if queue.len() >= max {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Dynamic import host queue exhausted",
                ));
            }
            let cap = &request.capability;
            let roots = [
                vm.root(cap.promise.clone()),
                vm.root(cap.resolve.clone()),
                vm.root(cap.reject.clone()),
            ];
            queue.push_back(Queued { realm: vm.current_realm(), request, roots });
            Ok(())
        });
        let modules = modules
            .into_iter()
            .map(|((identifier, kind), module)| ((vm.current_realm(), fixture_identifier(&identifier).to_owned(), kind), module))
            .collect();
        Ok(Self {
            source_name,
            sources,
            modules,
            queued,
        })
    }

    pub(super) fn step(&mut self, vm: &mut Vm) -> Result<bool, JsError> {
        let queued = self.queued.borrow_mut().pop_front();
        let Some(queued) = queued else {
            return Ok(false);
        };
        vm.with_realm(queued.realm, |vm| self.process(vm, queued))?;
        Ok(true)
    }

    fn process(&mut self, vm: &mut Vm, queued: Queued) -> Result<(), JsError> {
        let result = self.load(vm, &queued.request).and_then(|module| {
            vm.settle_source_text_import(module, queued.request.phase, queued.request.capability.clone())
        });
        if let Err(error) = result {
            return reject(vm, queued, error);
        }
        for root in queued.roots {
            vm.unroot(root);
        }
        Ok(())
    }

    fn load(&mut self, vm: &mut Vm, request: &DynamicImportRequest) -> Result<ModuleId, JsError> {
        let referrer = request.referrer.as_ref().map(JsString::to_string_lossy);
        let name = fixture_path(
            referrer.as_deref().unwrap_or(&self.source_name),
            &request.specifier,
        )?;
        let module = self.module(vm, name, request.module_type)?;
        if request.phase == rust_browser_javascript::ImportPhase::Source { return Ok(module); }
        vm.link_module_with_requests(module, |vm, importer, request| {
            let referrer = vm.module_identifier(importer)?.to_string_lossy();
            self.module(vm, fixture_path(&referrer, &request.specifier)?, request.module_type()?)
        })?;
        Ok(module)
    }

    fn module(&mut self, vm: &mut Vm, name: String, kind: ModuleType) -> Result<ModuleId, JsError> {
        let cache_key = (vm.current_realm(), name.clone(), kind);
        if let Some(&existing) = self.modules.get(&cache_key) {
            return Ok(existing);
        }
        if self.modules.len() >= 1024 {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Dynamic module record limit exhausted",
            ));
        }
        let source = self.sources.get(&name).ok_or_else(|| {
            if name.rsplit('/').next().is_some_and(|name| name.contains("_FIXTURE")) {
                JsError::new(ErrorKind::UnsupportedFeature, "Module fixture is outside the supplied bundle")
            } else {
                JsError::type_error(format!("Cannot load module {name}"))
            }
        })?;
        let module = crate::module_host::parse_fixture(vm, source, &name, kind)?;
        vm.root_module(module)?;
        self.modules.insert(cache_key, module);
        Ok(module)
    }
}

fn fixture_identifier(identifier: &str) -> &str {
    identifier
        .rsplit_once('#')
        .map_or(identifier, |(path, _)| path)
}

fn fixture_path(referrer: &str, specifier: &JsString) -> Result<String, JsError> {
    let unsupported = || {
        JsError::new(
            ErrorKind::UnsupportedFeature,
            "Dynamic import requires a relative source-text fixture inside the pinned tree",
        )
    };
    if specifier.len() > 8192 {
        return Err(JsError::new(
            ErrorKind::ExecutionLimit,
            "Dynamic module specifier exceeds the fixture host limit",
        ));
    }
    let specifier = String::from_utf16(specifier.as_utf16()).map_err(|_| unsupported())?;
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        if specifier.contains(':') || specifier.starts_with('/') {
            return Err(unsupported());
        }
        return Err(JsError::type_error("Unresolvable bare module specifier"));
    }
    if specifier.contains(['\\', '\0']) {
        return Err(JsError::type_error("Invalid module specifier"));
    }
    let referrer = fixture_identifier(referrer);
    let (directory, _) = referrer.rsplit_once('/').ok_or_else(unsupported)?;
    let mut components: Vec<_> = directory.split('/').collect();
    if components.first() != Some(&"test") {
        return Err(unsupported());
    }
    for component in specifier.split('/') {
        match component {
            "." | "" => {}
            ".." if components.len() > 1 => {
                components.pop();
            }
            ".." => return Err(unsupported()),
            name => components.push(name),
        }
    }
    Ok(components.join("/"))
}

fn reject(vm: &mut Vm, queued: Queued, error: JsError) -> Result<(), JsError> {
    let result = if matches!(
        error.kind,
        ErrorKind::UnsupportedFeature
            | ErrorKind::ExecutionLimit
            | ErrorKind::HeapLimit
            | ErrorKind::InternalError
    ) {
        Err(error)
    } else {
        let reason = match error.thrown.clone() {
            Some(reason) => Ok(reason),
            None => vm.error_value(&error),
        };
        reason.and_then(|reason| {
            vm.call(
                queued.request.capability.reject,
                Value::Undefined,
                &[reason],
            )
            .map(|_| ())
        })
    };
    for id in queued.roots {
        vm.unroot(id);
    }
    result
}

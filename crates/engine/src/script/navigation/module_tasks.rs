//! Host work for import() and suspended module evaluations.
use super::module_graph::{ModuleGraph, ModuleKey, Readiness};
use super::{ResourceQueue, ScriptRuntime};
use crate::network::CredentialsMode;
use crate::script::{
    JsError, RootId, Shared, Value, Vm, diagnostic, document_base, drain_microtask_queue, fatal,
    mutation_observers, report_unhandled,
};
use rust_browser_javascript::{DynamicImportRequest, ModuleId, ModuleStatus, PromiseCapability};
use std::sync::Arc;
use url::Url;

const IMPORT_COUNT: usize = 192;
const IMPORT_STRING_BYTES: usize = 2 * 1024 * 1024;
const IMPORT_ITEM_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub(in crate::script) struct ImportRequests {
    queued: Vec<Request>,
    count: usize,
    bytes: usize,
}
impl ImportRequests {
    pub(in crate::script) fn has_pending(&self) -> bool {
        !self.queued.is_empty()
    }
}

struct Request {
    from_module: Option<ModuleId>,
    base: Arc<Url>,
    specifier: String,
    phase: rust_browser_javascript::ImportPhase,
    kind: rust_browser_javascript::ModuleType,
    capability: PromiseCapability,
    roots: [RootId; 2],
}
impl Request {
    fn reject(self, vm: &mut Vm, error: JsError) -> Result<(), JsError> {
        let outcome = (|| {
            let reason = match &error.thrown {
                Some(value) => value.clone(),
                None => vm.error_value(&error)?,
            };
            vm.call(self.capability.reject.clone(), Value::Undefined, &[reason])
                .map(|_| ())
        })();
        self.release(vm);
        outcome
    }

    fn release(self, vm: &mut Vm) {
        for root in self.roots {
            vm.unroot(root);
        }
    }
}

struct Queued {
    request: Request,
    graph: Result<ModuleKey, String>,
}

#[derive(Default)]
pub(super) struct ModuleTasks {
    waiting: Vec<Queued>,
    scripts: Vec<ModuleId>,
}

impl ModuleTasks {
    pub(super) fn begin(
        &mut self,
        realm: &ScriptRuntime,
        graph: &mut ModuleGraph,
        client: &Arc<Url>,
        resources: &mut ResourceQueue,
    ) -> bool {
        let queued = std::mem::take(&mut realm.state.borrow_mut().import_requests.queued);
        let started = !queued.is_empty();
        for request in queued {
            let credentials = request
                .from_module
                .and_then(|id| graph.credentials(id))
                .unwrap_or(CredentialsMode::SameOrigin);
            let result = graph
                .resolve_specifier(&request.specifier, &request.base)
                .and_then(|url| graph.external_typed(url, client, credentials, None, resources, request.kind));
            self.waiting.push(Queued {
                request,
                graph: result,
            });
        }
        started
    }

    pub(super) fn stop_queued(&mut self, realm: &ScriptRuntime) {
        let queued = std::mem::take(&mut realm.state.borrow_mut().import_requests.queued);
        self.waiting
            .extend(queued.into_iter().map(|request| Queued {
                request,
                graph: Err("Module fetch stopped".into()),
            }));
        for item in &mut self.waiting {
            item.graph = Err("Module fetch stopped".into());
        }
    }

    fn waiting_index(&self, graph: &ModuleGraph) -> Option<usize> {
        self.waiting.iter().position(|item| match &item.graph {
            Ok(key) => !matches!(graph.readiness(*key), Readiness::Pending),
            Err(_) => true,
        })
    }

    pub(super) fn ready(&self, graph: &ModuleGraph, realm: &ScriptRuntime) -> bool {
        self.scripts
            .iter()
            .copied()
            .any(|module| evaluation_finished(&realm.vm, module))
            || self.waiting_index(graph).is_some()
    }

    pub(super) fn waiting_on_graph(&self) -> bool {
        !self.waiting.is_empty()
    }

    pub(super) fn execute_script(
        &mut self,
        graph: &ModuleGraph,
        realm: &mut ScriptRuntime,
        module: ModuleId,
    ) {
        match evaluate(graph, realm, module) {
            Ok(None) => self.scripts.push(module),
            Ok(Some(result)) => record_script_result(realm, result),
            Err(error) => record_script_result(realm, Err(error)),
        }
        report_unhandled(&mut realm.vm, &realm.state);
        realm.vm.collect_garbage();
    }

    pub(super) fn progress(&mut self, graph: &ModuleGraph, realm: &mut ScriptRuntime) -> bool {
        if let Some(index) = self
            .scripts
            .iter()
            .position(|module| evaluation_finished(&realm.vm, *module))
        {
            let module = self.scripts.remove(index);
            match realm.vm.module_evaluation_result(module) {
                Ok(Some(result)) => record_script_result(realm, result),
                Err(error) => record_script_result(realm, Err(error)),
                Ok(None) => {}
            }
        } else if let Some(index) = self.waiting_index(graph) {
            let item = self.waiting.remove(index);
            let readiness = match item.graph {
                Ok(key) => graph.readiness(key),
                Err(error) => Readiness::Failed(error.into()),
            };
            match readiness {
                Readiness::Failed(error) => {
                    reject_import(realm, item.request, JsError::type_error(error.to_string()))
                }
                Readiness::Ready(module) => start_import(realm, graph, item.request, module),
                Readiness::Pending => unreachable!("only ready graphs are selected"),
            }
        } else {
            return false;
        }
        report_unhandled(&mut realm.vm, &realm.state);
        realm.vm.collect_garbage();
        true
    }
}

fn evaluation_finished(vm: &Vm, module: ModuleId) -> bool {
    !matches!(vm.module_status(module), Ok(ModuleStatus::Evaluating))
}

fn evaluate(
    graph: &ModuleGraph,
    realm: &mut ScriptRuntime,
    module: ModuleId,
) -> Result<Option<Result<(), JsError>>, JsError> {
    let state = realm.state.clone();
    module_context(realm, |vm| {
        graph.link(vm, module)?;
        vm.evaluate_module(module)?;
        vm.observe_module_evaluation(module)?;
        drain_microtask_queue(vm, &state)?;
        vm.module_evaluation_result(module)
    })
}

fn module_context<T>(
    realm: &mut ScriptRuntime,
    task: impl FnOnce(&mut Vm) -> Result<T, JsError>,
) -> Result<T, JsError> {
    let state = realm.state.clone();
    let current_script = {
        let mut state = state.borrow_mut();
        state.ignore_destructive_writes += 1;
        state.current_script.take()
    };
    let result = realm.vm.with_task(task);
    {
        let mut state = state.borrow_mut();
        state.current_script = current_script;
        state.ignore_destructive_writes -= 1;
    }
    result
}

fn record_script_result(realm: &mut ScriptRuntime, result: Result<(), JsError>) {
    match result {
        Ok(()) => realm.state.borrow_mut().executed += 1,
        Err(error) => {
            if fatal(&error) {
                mutation_observers::discard_jobs(&mut realm.vm, &realm.state);
            }
            diagnostic(&realm.state, format!("Module: {error}"));
        }
    }
}

fn start_import(
    realm: &mut ScriptRuntime,
    graph: &ModuleGraph,
    request: Request,
    module: ModuleId,
) {
    let state = realm.state.clone();
    let registered = module_context(realm, |vm| {
        if request.phase != rust_browser_javascript::ImportPhase::Source { graph.link(vm, module)?; }
        vm.settle_source_text_import(module, request.phase, request.capability.clone())
    });
    if let Err(error) = registered {
        reject_import(realm, request, error);
        return;
    }
    // The VM reaction now traces both resolvers and preserves their FIFO order.
    request.release(&mut realm.vm);
    let result = realm.vm.with_task(|vm| drain_microtask_queue(vm, &state));
    report_import_failure(realm, result);
}

fn reject_import(realm: &mut ScriptRuntime, request: Request, error: JsError) {
    let state = realm.state.clone();
    let result = realm.vm.with_task(|vm| {
        request.reject(vm, error)?;
        drain_microtask_queue(vm, &state)
    });
    report_import_failure(realm, result);
}

fn report_import_failure(realm: &mut ScriptRuntime, result: Result<(), JsError>) {
    if let Err(error) = result {
        if fatal(&error) {
            mutation_observers::discard_jobs(&mut realm.vm, &realm.state);
        }
        diagnostic(&realm.state, format!("Dynamic import: {error}"));
    }
}

pub(in crate::script) fn install(vm: &mut Vm, state: &Shared) {
    let host = state.clone();
    vm.set_dynamic_import_hook(move |vm, request| capture(vm, &host, request));
}

fn capture(vm: &mut Vm, host: &Shared, request: DynamicImportRequest) -> Result<(), JsError> {
    let mut state = host.borrow_mut();
    if !state.navigation_attached {
        return Err(JsError::type_error(
            "Dynamic import requires an active document session",
        ));
    }
    if request.specifier.len() > IMPORT_ITEM_BYTES
        || request
            .referrer
            .as_ref()
            .is_some_and(|value| value.len() > IMPORT_ITEM_BYTES)
        || state.import_requests.count >= IMPORT_COUNT
    {
        return Err(JsError::type_error("Dynamic import request limit exceeded"));
    }
    let base = if let Some(referrer) = request.referrer {
        vm.charge_work(referrer.len())?;
        Url::parse(&referrer.to_string_lossy())
            .map_err(|_| JsError::type_error("Invalid import referrer"))?
    } else {
        vm.charge_work(state.document.nodes.len())?;
        document_base(&state)
    };
    vm.charge_work(request.specifier.len())?;
    let specifier = request.specifier.to_string_lossy();
    let bytes = specifier.len().saturating_add(base.as_str().len());
    let pending = &mut state.import_requests;
    if pending.bytes.saturating_add(bytes) > IMPORT_STRING_BYTES {
        return Err(JsError::type_error(
            "Dynamic import specifier or URL budget exceeded",
        ));
    }
    vm.charge_work(bytes)?;
    let capability = request.capability;
    let roots = [
        vm.root(capability.resolve.clone()),
        vm.root(capability.reject.clone()),
    ];
    pending.queued.push(Request {
        from_module: request.referrer_module,
        phase: request.phase,
        kind: request.module_type,
        base: Arc::new(base),
        specifier,
        capability,
        roots,
    });
    pending.bytes += bytes;
    pending.count += 1;
    Ok(())
}

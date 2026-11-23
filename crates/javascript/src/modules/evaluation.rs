//! Evaluate module graphs using explicit DFS and asynchronous dependency edges.
use super::{ModuleId, ModuleStatus};
use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm, dynamic_import::SourceReferrer, heap::ObjectKind,
    promise::ReactionPair, vm::Completion,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

struct EvaluationFrame {
    module: ModuleId,
    edge: usize,
    returned_from: Option<ModuleId>,
}
impl EvaluationFrame {
    fn new(module: ModuleId) -> Self {
        Self {
            module,
            edge: 0,
            returned_from: None,
        }
    }
}

impl Vm {
    pub(super) fn module_cycle_root(&self, id: ModuleId) -> Result<ModuleId, JsError> {
        Ok(self.module_data(id)?.cycle_root.unwrap_or(id))
    }

    /// Read an evaluation failure without marking its promise handled.
    pub fn module_evaluation_error(&self, id: ModuleId) -> Result<Option<JsError>, JsError> {
        let data = self.module_data(self.module_cycle_root(id)?)?;
        Ok(data.error.as_deref().cloned())
    }

    /// Observe the graph result after execution or a promise checkpoint.
    /// Pending graphs return None. An observed rejection is marked handled.
    pub fn module_evaluation_result(
        &mut self,
        id: ModuleId,
    ) -> Result<Option<Result<(), JsError>>, JsError> {
        self.with_task(|vm| {
            let data = vm.module_data(vm.module_cycle_root(id)?)?;
            let result = match data.status {
                ModuleStatus::Evaluated => Ok(()),
                ModuleStatus::Errored => Err((**data.error.as_ref().unwrap()).clone()),
                _ => return Ok(None),
            };
            if result.is_err() {
                vm.observe_module_evaluation(id)?;
            }
            Ok(Some(result))
        })
    }

    /// Mark the intrinsic graph promise handled, including before it settles.
    /// Embedders use this when they report module script errors themselves.
    pub fn observe_module_evaluation(&mut self, id: ModuleId) -> Result<(), JsError> {
        self.with_task(|vm| {
            let data = vm.module_data(vm.module_cycle_root(id)?)?;
            if let Some(promise) = data.evaluation.as_ref().and_then(Value::as_object) {
                vm.consume_work(vm.unhandled_promises.len())?;
                if let ObjectKind::Promise(data) = &mut vm.heap.object_mut(promise)?.kind {
                    data.handled = true;
                }
                vm.unhandled_promises.retain(|id| *id != promise);
            }
            Ok(())
        })
    }

    pub fn evaluate_module(&mut self, id: ModuleId) -> Result<Value, JsError> {
        self.with_task(|vm| vm.evaluate_module_task(id))
    }

    fn evaluate_module_task(&mut self, id: ModuleId) -> Result<Value, JsError> {
        let id = self.module_cycle_root(id)?;
        let data = self.module_data(id)?;
        let status = data.status;
        if matches!(status, ModuleStatus::Unlinked | ModuleStatus::Linking) {
            return Err(JsError::type_error(
                "Module must be linked before evaluation",
            ));
        }
        if let Some(error) = &data.error
            && error.uncatchable()
        {
            return Err((**error).clone());
        }
        if let Some(promise) = &data.evaluation {
            return Ok(promise.clone());
        }
        if self.module_evaluation_active {
            return Err(JsError::type_error("Reentrant module evaluation"));
        }
        let error = data.error.clone();
        let promise = self.new_promise_capability()?.promise;
        let promise_id = promise.as_object().unwrap();
        self.module_data_mut(id)?.evaluation = Some(promise.clone());
        let result = match status {
            ModuleStatus::Evaluated => self.settle_promise(promise_id, Value::Undefined, false),
            ModuleStatus::Errored => {
                let error = error.unwrap();
                self.settle_promise(promise_id, error.thrown.clone().unwrap_or_default(), true)
            }
            ModuleStatus::Linked => {
                self.module_evaluation_active = true;
                let result = self.evaluate_graph(id);
                self.module_evaluation_active = false;
                result
            }
            ModuleStatus::Evaluating => Ok(()),
            _ => unreachable!(),
        };
        if let Err(error) = result {
            self.module_data_mut(id)?.evaluation = None;
            return Err(error);
        }
        Ok(promise)
    }

    fn evaluate_graph(&mut self, root: ModuleId) -> Result<(), JsError> {
        let mut indices = BTreeMap::<ModuleId, (usize, usize)>::new();
        let mut frames = Vec::<EvaluationFrame>::new();
        let mut components = Vec::<ModuleId>::new();
        self.reserve_allocation(256)?;
        self.module_data_mut(root)?.status = ModuleStatus::Evaluating;
        indices.insert(root, (0, 0));
        frames.push(EvaluationFrame::new(root));
        components.push(root);
        let execution = (|| {
            while let Some(frame) = frames.last_mut() {
                self.tick()?;
                let current = frame.module;
                if let Some(dependency) = frame.returned_from.take() {
                    self.note_evaluated_dependency(&mut indices, current, dependency)?;
                    continue;
                }
                let data = self.module_data(current)?;
                if let Some(&dependency) = data.dependencies.get(frame.edge) {
                    frame.edge += 1;
                    if self.module_data(dependency)?.status == ModuleStatus::Linked {
                        self.reserve_allocation(256)?;
                        self.module_data_mut(dependency)?.status = ModuleStatus::Evaluating;
                        let index = indices.len();
                        indices.insert(dependency, (index, index));
                        frame.returned_from = Some(dependency);
                        frames.push(EvaluationFrame::new(dependency));
                        components.push(dependency);
                    } else {
                        self.note_evaluated_dependency(&mut indices, current, dependency)?;
                    }
                    continue;
                }
                let (has_await, waiting) =
                    (data.code.body.can_suspend, data.pending_dependencies > 0);
                if has_await || waiting {
                    self.module_async_order =
                        self.module_async_order.checked_add(1).ok_or_else(|| {
                            JsError::new(
                                ErrorKind::ExecutionLimit,
                                "Module async order budget exhausted",
                            )
                        })?;
                    self.module_data_mut(current)?.async_order = Some(self.module_async_order);
                    if !waiting {
                        self.start_async_module(current)?;
                    }
                } else {
                    self.run_sync_module(current)?;
                }

                frames.pop();
                let (index, ancestor) = indices[&current];
                if index == ancestor {
                    self.finish_module_cycle(&mut components, current)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = execution {
            self.fail_module_evaluations(components, Some(root), error)?;
        }
        Ok(())
    }

    fn note_evaluated_dependency(
        &mut self,
        indices: &mut BTreeMap<ModuleId, (usize, usize)>,
        current: ModuleId,
        dependency: ModuleId,
    ) -> Result<(), JsError> {
        let data = self.module_data(dependency)?;
        let dependency = if data.status == ModuleStatus::Evaluating && data.cycle_root.is_none() {
            let ancestor = indices[&dependency].1;
            let index = &mut indices.get_mut(&current).unwrap().1;
            *index = (*index).min(ancestor);
            dependency
        } else {
            data.cycle_root.unwrap_or(dependency)
        };
        let data = self.module_data(dependency)?;
        if let Some(error) = &data.error {
            return Err((**error).clone());
        }
        if !matches!(
            data.status,
            ModuleStatus::Evaluating | ModuleStatus::Evaluated
        ) {
            return Err(JsError::new(
                ErrorKind::InternalError,
                "Unlinked evaluation dependency",
            ));
        }
        if data.async_order.is_some() {
            if data.async_parents.len() >= self.limits.max_pending_jobs {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Async module dependency budget exhausted",
                ));
            }
            self.reserve_allocation(128)?;
            self.module_data_mut(dependency)?
                .async_parents
                .push(current);
            self.module_data_mut(current)?.pending_dependencies += 1;
        }
        Ok(())
    }

    fn finish_module_cycle(
        &mut self,
        components: &mut Vec<ModuleId>,
        root: ModuleId,
    ) -> Result<(), JsError> {
        let data = self.module_data(root)?;
        if data.async_order.is_none()
            && let Some(promise) = data.evaluation.as_ref().and_then(Value::as_object)
        {
            self.settle_promise(promise, Value::Undefined, false)?;
        }
        loop {
            let member = components.pop().unwrap();
            let data = self.module_data_mut(member)?;
            data.cycle_root = Some(root);
            if data.async_order.is_none() {
                data.status = ModuleStatus::Evaluated;
            }
            if member == root {
                break;
            }
        }
        Ok(())
    }

    fn run_sync_module(&mut self, module: ModuleId) -> Result<(), JsError> {
        let data = self.module_data(module)?;
        let body = data.code.body.clone();
        let env = data.environment.unwrap();
        let result = self.with_referrer(Some(SourceReferrer::Module(module)), |vm| {
            vm.run_prepared_chunk(&body, env)
        })?;
        match result {
            Completion::Normal(_) => Ok(()),
            _ => Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid module completion",
            )),
        }
    }

    fn start_async_module(&mut self, module: ModuleId) -> Result<(), JsError> {
        let data = self.module_data(module)?;
        let body = data.code.body.clone();
        let env = data.environment.unwrap();
        let capability = self.new_promise_capability()?;
        self.register_reaction(
            capability.promise.as_object().unwrap(),
            ReactionPair::ModuleEvaluation(module),
        )?;
        self.with_referrer(Some(SourceReferrer::Module(module)), |vm| {
            vm.start_async_body(module.0, body, env, capability, true)
        })
    }

    pub(crate) fn finish_async_module(
        &mut self,
        module: ModuleId,
        value: Value,
        rejected: bool,
    ) -> Result<(), JsError> {
        if self.module_data(module)?.status != ModuleStatus::Evaluating {
            return Ok(());
        }
        if rejected {
            let error = match self.thrown_error(value) {
                Ok(error) | Err(error) => error,
            };
            return self.fail_module_evaluations(vec![module], None, error);
        }
        let mut ready = BTreeSet::new();
        if let Err(error) = self.complete_async_module(module, &mut ready) {
            return self.fail_module_evaluations(vec![module], None, error);
        }
        while let Some((_, ancestor)) = ready.pop_first() {
            let result = (|| {
                self.tick()?;
                if self.module_data(ancestor)?.status != ModuleStatus::Evaluating
                    || self
                        .module_data(self.module_cycle_root(ancestor)?)?
                        .error
                        .is_some()
                {
                    return Ok(());
                }
                if self.module_data(ancestor)?.code.body.can_suspend {
                    self.start_async_module(ancestor)
                } else {
                    self.run_sync_module(ancestor)?;
                    self.complete_async_module(ancestor, &mut ready)
                }
            })();
            if let Err(error) = result {
                let mut failed = vec![ancestor];
                if error.uncatchable() {
                    failed.extend(ready.iter().map(|(_, id)| *id));
                }
                self.fail_module_evaluations(failed, None, error)?;
            }
        }
        Ok(())
    }

    fn complete_async_module(
        &mut self,
        module: ModuleId,
        ready: &mut BTreeSet<(u64, ModuleId)>,
    ) -> Result<(), JsError> {
        let data = self.module_data(module)?;
        let parents = data.async_parents.len();
        let promise = data.evaluation.as_ref().and_then(Value::as_object);
        self.consume_work(parents)?;
        self.reserve_allocation(parents.saturating_mul(128))?;
        if let Some(promise) = promise {
            self.settle_promise(promise, Value::Undefined, false)?;
        }
        let data = self.module_data_mut(module)?;
        data.async_order = None;
        data.status = ModuleStatus::Evaluated;
        for parent in std::mem::take(&mut data.async_parents) {
            let root = self.module_cycle_root(parent)?;
            if self.module_data(root)?.error.is_some() {
                continue;
            }
            let parent_data = self.module_data_mut(parent)?;
            if let Some(order) = parent_data.async_order {
                parent_data.pending_dependencies -= 1;
                if parent_data.pending_dependencies == 0 {
                    // Every dependency has an earlier order than the modules
                    // waiting for it, so inserting newly ready modules here
                    // preserves the same order across synchronous descendants.
                    ready.insert((order, parent));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn record_async_execution_limit(&mut self, owner: ObjectId, error: &JsError) {
        if error.uncatchable()
            && let Ok(object) = self.heap.object(owner)
            && matches!(object.kind, ObjectKind::Module(_))
        {
            let module = ModuleId(owner, self.module_realm);
            let _ = self.fail_module_evaluations(vec![module], None, error.clone());
        }
    }

    fn fail_module_evaluations(
        &mut self,
        mut pending: Vec<ModuleId>,
        new_cycle_root: Option<ModuleId>,
        mut error: JsError,
    ) -> Result<(), JsError> {
        if !error.uncatchable() && error.thrown.is_none() {
            match self.error_value(&error) {
                Ok(reason) => error.thrown = Some(reason),
                Err(fatal) => error = fatal,
            }
        }
        let mut error = Rc::new(error);
        while let Some(member) = pending.pop() {
            let data = self.module_data_mut(member)?;
            if data.status == ModuleStatus::Errored {
                continue;
            }
            if data.cycle_root.is_none() {
                data.cycle_root = new_cycle_root;
            }
            data.status = ModuleStatus::Errored;
            data.pending_dependencies = 0;
            data.async_order = None;
            pending.extend(std::mem::take(&mut data.async_parents).into_iter().rev());
            let promise = data.evaluation.as_ref().and_then(Value::as_object);
            if !error.uncatchable()
                && let Some(promise) = promise
                && let Err(fatal) =
                    self.settle_promise(promise, error.thrown.clone().unwrap_or_default(), true)
            {
                error = Rc::new(fatal);
            }
            self.module_data_mut(member)?.error = Some(error.clone());
        }
        if error.uncatchable() {
            Err((*error).clone())
        } else {
            Ok(())
        }
    }
}

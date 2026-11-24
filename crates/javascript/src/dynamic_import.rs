//! Dynamic import preserves source identity while the embedding loads its graph.
use crate::promise::ReactionPair;
use crate::{
    JsError, JsString, ModuleId, ObjectId, PromiseCapability, Value, Vm, heap::ObjectKind,
};
use std::rc::Rc;

/// A dynamic import to resolve against the active script or module identifier.
/// The host must root capability values while retaining this request across GC.
#[derive(Clone, Debug)]
pub struct DynamicImportRequest {
    pub referrer: Option<JsString>,
    pub referrer_module: Option<ModuleId>,
    pub specifier: JsString,
    pub capability: PromiseCapability,
}

pub(crate) type DynamicImportHook =
    Rc<dyn Fn(&mut Vm, DynamicImportRequest) -> Result<(), JsError>>;

#[derive(Clone)]
pub(crate) enum SourceReferrer {
    Script(JsString),
    Module(ModuleId),
}

impl Vm {
    /// Install the host load hook. It should enqueue the request and return.
    /// As with native functions, retained JavaScript values require explicit roots.
    pub fn set_dynamic_import_hook<F>(&mut self, hook: F)
    where
        F: Fn(&mut Vm, DynamicImportRequest) -> Result<(), JsError> + 'static,
    {
        self.realm.dynamic_import_hook = Some(Rc::new(hook));
    }

    /// Evaluate an already linked module and settle the import capability from
    /// its graph Promise. The VM retains the capability in the graph reaction.
    pub fn settle_import_after_evaluation(
        &mut self,
        module: ModuleId,
        capability: PromiseCapability,
    ) -> Result<(), JsError> {
        self.with_task(|vm| {
            let evaluation = vm.evaluate_module(module)?;
            vm.register_reaction(
                evaluation.as_object().unwrap(),
                ReactionPair::DynamicImport { module, capability },
            )
        })
    }

    pub(crate) fn with_referrer<T>(
        &mut self,
        referrer: Option<SourceReferrer>,
        run: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let previous = std::mem::replace(&mut self.realm.active_referrer, referrer);
        let result = run(self);
        self.realm.active_referrer = previous;
        result
    }

    pub(crate) fn referrer_for_owner(
        &self,
        owner: ObjectId,
    ) -> Result<Option<SourceReferrer>, JsError> {
        Ok(match &self.heap.object(owner)?.kind {
            ObjectKind::Function { referrer, .. } => referrer.clone(),
            ObjectKind::Module(_) => Some(SourceReferrer::Module(ModuleId(
                owner,
                self.realm.module_realm,
            ))),
            _ => None,
        })
    }

    pub(crate) fn dynamic_import(&mut self, value: Value) -> Result<Value, JsError> {
        let capability = self.new_promise_capability()?;
        let result = (|| {
            let specifier = self.to_string(&value)?;
            self.account_value(Value::String(specifier.clone()))?;
            let (referrer, referrer_module) = match self.realm.active_referrer.as_ref() {
                Some(SourceReferrer::Script(identifier)) => (Some(identifier.clone()), None),
                Some(SourceReferrer::Module(module)) => {
                    (Some(self.module_identifier(*module)?), Some(*module))
                }
                None => (None, None),
            };
            let hook = self
                .realm
                .dynamic_import_hook
                .clone()
                .ok_or_else(|| JsError::type_error("No dynamic import loader is installed"))?;
            hook(
                self,
                DynamicImportRequest {
                    referrer,
                    referrer_module,
                    specifier,
                    capability: capability.clone(),
                },
            )
        })();
        if let Err(error) = result {
            let reason = self.error_reason(error)?;
            self.invoke(capability.reject, Value::Undefined, &[reason])?;
        }
        Ok(capability.promise)
    }
}

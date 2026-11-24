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

    fn check_import_options(&mut self, options: Value) -> Result<(), JsError> {
        if options.is_undefined() {
            return Ok(());
        }
        if options.as_object().is_none() {
            return Err(JsError::type_error("Import options must be an object"));
        }
        let attributes = self.get_property(options, "with")?;
        if attributes.is_undefined() {
            return Ok(());
        }
        let Some(id) = attributes.as_object() else {
            return Err(JsError::type_error("Import attributes must be an object"));
        };
        // The current loader supports source-text JavaScript with no attribute keys.
        // Read all enumerable values in order before rejecting unsupported keys.
        let mut has_attributes = false;
        let mut invalid_value = false;
        for name in self.own_keys(attributes.clone(), false)? {
            let key = crate::PropertyKey::String(name);
            if !self.own_enumerability(id, &key)?.unwrap_or(false) {
                continue;
            }
            if !matches!(
                self.get_property_key(attributes.clone(), key)?,
                Value::String(_)
            ) {
                invalid_value = true;
            }
            has_attributes = true;
        }
        if invalid_value {
            return Err(JsError::type_error(
                "Import attributes require string values",
            ));
        }
        if has_attributes {
            return Err(JsError::type_error("Unsupported import attribute"));
        }
        Ok(())
    }

    pub(crate) fn dynamic_import(
        &mut self,
        phase: crate::parser::ImportPhase,
        value: Value,
        options: Value,
    ) -> Result<Value, JsError> {
        let capability = self.new_promise_capability()?;
        let result = (|| {
            let specifier = self.to_string(&value)?;
            self.account_value(Value::String(specifier.clone()))?;
            self.check_import_options(options)?;
            if phase != crate::parser::ImportPhase::Evaluation {
                return Err(JsError::new(
                    crate::ErrorKind::UnsupportedFeature,
                    "The module loader does not implement deferred or source-phase evaluation",
                ));
            }
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

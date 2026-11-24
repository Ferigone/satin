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
    pub phase: crate::ImportPhase,
    pub module_type: crate::ModuleType,
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
        self.realm.inherit_dynamic_import_hook = false;
        self.realm.dynamic_import_hook = Some(Rc::new(hook));
    }

    /// Install a loader which understands the invoking realm and keeps its
    /// module graphs isolated. Shadow realms may delegate to this host.
    pub fn set_inherited_dynamic_import_hook<F>(&mut self, hook: F)
    where
        F: Fn(&mut Vm, DynamicImportRequest) -> Result<(), JsError> + 'static,
    {
        self.set_dynamic_import_hook(hook);
        self.realm.inherit_dynamic_import_hook = true;
    }

    /// Complete a dynamic import once the host has loaded a JavaScript module.
    /// Source-text modules have no source-phase representation.
    pub fn settle_source_text_import(&mut self, module: ModuleId, phase: crate::ImportPhase, capability: PromiseCapability) -> Result<(), JsError> {
        match phase {
            crate::ImportPhase::Evaluation => self.settle_import_after_evaluation(module, capability),
            crate::ImportPhase::Source => Err(JsError::new(crate::ErrorKind::SyntaxError, "JavaScript modules have no source-phase representation")),
            crate::ImportPhase::Defer => Err(JsError::new(crate::ErrorKind::UnsupportedFeature, "Deferred module namespaces are not implemented")),
        }
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

    fn import_attributes(&mut self, options: Value) -> Result<std::collections::BTreeMap<JsString, JsString>, JsError> {
        if options.is_undefined() {
            return Ok(Default::default());
        }
        if options.as_object().is_none() {
            return Err(JsError::type_error("Import options must be an object"));
        }
        let attributes = self.get_property(options, "with")?;
        if attributes.is_undefined() {
            return Ok(Default::default());
        }
        let Some(id) = attributes.as_object() else {
            return Err(JsError::type_error("Import attributes must be an object"));
        };
        let mut entries = std::collections::BTreeMap::new();
        let mut invalid_value = false;
        for name in self.own_keys(attributes.clone(), false)? {
            let key = crate::PropertyKey::String(name);
            if !self.own_enumerability(id, &key)?.unwrap_or(false) {
                continue;
            }
            match self.get_property_key(attributes.clone(), key.clone())? {
                Value::String(value) => {
                    self.reserve_allocation(128)?;
                    let crate::PropertyKey::String(name) = key else { unreachable!() };
                    entries.insert(name, value);
                }
                _ => invalid_value = true,
            }
        }
        if invalid_value {
            return Err(JsError::type_error(
                "Import attributes require string values",
            ));
        }
        Ok(entries)
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
            let attributes = self.import_attributes(options)?;
            let request = crate::ModuleRequest { specifier, attributes };
            let module_type = request.module_type()?;
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
                    specifier: request.specifier,
                    phase,
                    module_type,
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

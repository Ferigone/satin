use crate::{
    ErrorKind, JsError, JsString, ObjectId, Value, Vm,
    heap::{ObjectKind, array_index},
};
use std::rc::Rc;

/// Read-only indexed properties backed by a live host collection.
///
/// The current indices are contiguous from zero through `indexed_len() - 1`.
/// Providers must return a value for each such index and must not run JavaScript
/// or mutate the collection while servicing a property lookup. Host metadata
/// carries its own resource limits. Any retained JavaScript values need explicit
/// VM roots or traced slots on the wrapper.
pub trait HostIndexedProperties {
    fn indexed_len(&self) -> Result<usize, JsError>;
    fn indexed_get(&self, vm: &mut Vm, index: u32) -> Result<Option<Value>, JsError>;
}

/// A legacy Web IDL object with writable named properties. Ordinary own
/// properties and properties on the prototype chain hide supported names.
/// Getters and key enumeration must not execute author code.
pub trait HostNamedProperties {
    fn names(&self) -> Vec<JsString>;
    fn get(&self, name: &JsString) -> Option<Value>;
    fn set(&self, vm: &mut Vm, name: JsString, value: Value) -> Result<(), JsError>;
    fn delete(&self, vm: &mut Vm, name: &JsString) -> Result<(), JsError>;
}

impl Vm {
    pub fn set_host_named_properties(
        &mut self,
        wrapper: Value,
        provider: Rc<dyn HostNamedProperties>,
    ) -> Result<(), JsError> {
        let id = wrapper
            .as_object()
            .ok_or_else(|| JsError::type_error("Named host target must be an object"))?;
        let object = self.heap.object_mut(id)?;
        if !matches!(object.kind, ObjectKind::Ordinary)
            || !object.extensible
            || object.indexed_properties.is_some()
            || object.named_properties.is_some()
        {
            return Err(JsError::type_error(
                "Object cannot accept named host properties",
            ));
        }
        object.named_properties = Some(provider);
        Ok(())
    }

    pub(crate) fn host_named_setter(
        &self,
        id: ObjectId,
    ) -> Result<Option<Rc<dyn HostNamedProperties>>, JsError> {
        Ok(self.heap.object(id)?.named_properties.clone())
    }

    pub(crate) fn host_named_definition(
        &self,
        id: ObjectId,
        name: &JsString,
    ) -> Result<Option<Rc<dyn HostNamedProperties>>, JsError> {
        let object = self.heap.object(id)?;
        Ok(object
            .named_properties
            .as_ref()
            .filter(|_| !object.properties.contains_key(name))
            .cloned())
    }

    fn named_unshadowed(&self, id: ObjectId, name: &JsString) -> Result<bool, JsError> {
        let object = self.heap.object(id)?;
        if object.properties.contains_key(name) {
            return Ok(false);
        }
        let mut current = object.prototype;
        for _ in 0..1024 {
            let Some(id) = current else {
                return Ok(true);
            };
            let object = self.heap.object(id)?;
            if matches!(&object.kind, ObjectKind::ModuleNamespace(ns) if ns.exports.contains_key(name))
                || !matches!(object.kind, ObjectKind::ModuleNamespace(_))
                    && self.own_descriptor(id, name)?.is_some()
                || self.host_indexed_has(id, name)?
                || object
                    .named_properties
                    .as_ref()
                    .is_some_and(|names| names.get(name).is_some())
            {
                return Ok(false);
            }
            current = object.prototype;
        }
        Err(JsError::new(
            ErrorKind::RangeError,
            "Prototype chain limit exceeded",
        ))
    }

    pub(crate) fn host_named_value(
        &self,
        id: ObjectId,
        name: &JsString,
    ) -> Result<Option<Value>, JsError> {
        let value = self
            .heap
            .object(id)?
            .named_properties
            .as_ref()
            .and_then(|properties| properties.get(name));
        if value.is_some() && self.named_unshadowed(id, name)? {
            Ok(value)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn host_named_keys(&mut self, id: ObjectId) -> Result<Vec<JsString>, JsError> {
        let Some(provider) = self.host_named_setter(id)? else {
            return Ok(Vec::new());
        };
        let mut visible = Vec::new();
        for name in provider.names() {
            self.tick()?;
            if self.named_unshadowed(id, &name)? {
                self.account_value(Value::String(name.clone()))?;
                visible.push(name);
            }
        }
        Ok(visible)
    }

    /// Install a live index provider on an extensible ordinary object with no
    /// existing own indexed properties. The provider is held by the VM heap.
    pub fn set_host_indexed_properties(
        &mut self,
        wrapper: Value,
        provider: Rc<dyn HostIndexedProperties>,
    ) -> Result<(), JsError> {
        let id = wrapper
            .as_object()
            .ok_or_else(|| JsError::type_error("Indexed host target must be an object"))?;
        let object = self.heap.object_mut(id)?;
        if !matches!(object.kind, ObjectKind::Ordinary)
            || !object.extensible
            || object.indexed_properties.is_some()
            || object.named_properties.is_some()
            || object
                .properties
                .keys()
                .any(|key| array_index(key).is_some())
        {
            return Err(JsError::type_error(
                "Object cannot accept indexed host properties",
            ));
        }
        object.indexed_properties = Some(provider);
        Ok(())
    }

    pub(crate) fn host_indexed_key(
        &self,
        id: ObjectId,
        key: &JsString,
    ) -> Result<Option<u32>, JsError> {
        if self.heap.object(id)?.indexed_properties.is_none() {
            return Ok(None);
        }
        Ok(array_index(key))
    }

    pub(crate) fn host_indexed_len(&self, id: ObjectId) -> Result<usize, JsError> {
        let Some(provider) = &self.heap.object(id)?.indexed_properties else {
            return Ok(0);
        };
        let len = provider.indexed_len()?;
        if len > self.limits.max_array_length as usize {
            return Err(JsError::new(
                ErrorKind::ExecutionLimit,
                "Host index count budget exhausted",
            ));
        }
        Ok(len)
    }

    fn host_supported_index(&self, id: ObjectId, key: &JsString) -> Result<Option<u32>, JsError> {
        let Some(index) = self.host_indexed_key(id, key)? else {
            return Ok(None);
        };
        Ok(((index as usize) < self.host_indexed_len(id)?).then_some(index))
    }

    pub(crate) fn host_indexed_has(&self, id: ObjectId, key: &JsString) -> Result<bool, JsError> {
        Ok(self.host_supported_index(id, key)?.is_some())
    }

    pub(crate) fn host_indexed_get(
        &mut self,
        id: ObjectId,
        key: &JsString,
    ) -> Result<Option<Value>, JsError> {
        let Some(index) = self.host_supported_index(id, key)? else {
            return Ok(None);
        };
        let provider = self
            .heap
            .object(id)?
            .indexed_properties
            .as_ref()
            .unwrap()
            .clone();
        self.with_task(|vm| {
            vm.tick()?;
            let value = provider.indexed_get(vm, index)?.ok_or_else(|| {
                JsError::new(
                    ErrorKind::InternalError,
                    "Host did not return a supported index",
                )
            })?;
            vm.account_value(value).map(Some)
        })
    }

    pub(crate) fn prevent_extensions(&mut self, id: ObjectId) -> Result<bool, JsError> {
        if self.proxy(id) {
            return self.proxy_prevent(id);
        }
        if !self.typed_can_prevent_extensions(id)? {
            return Ok(false);
        }
        let object = self.heap.object_mut(id)?;
        if object.indexed_properties.is_some() || object.named_properties.is_some() {
            return Ok(false);
        }
        object.extensible = false;
        Ok(true)
    }
}

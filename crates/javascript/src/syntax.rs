//! Shared mechanisms for source syntax: template sites and CopyDataProperties.
//! Bytecode sites own text; the VM cache contains weak site identities and traced values.
use crate::{
    JsError, PropertyKey, Value, Vm,
    bytecode::TemplateSite,
    heap::{ObjectKind, PropertyKind},
};
use std::rc::Rc;
impl Vm {
    pub(crate) fn copy_data_properties(
        &mut self,
        target: Value,
        source: Value,
        excluded: &[PropertyKey],
    ) -> Result<(), JsError> {
        if source.is_nullish() {
            return Ok(());
        }
        let source = self.coerce_object(source)?;
        for key in self.own_property_keys(source.clone())? {
            self.tick()?;
            if let PropertyKey::String(text) = &key {
                self.consume_work(text.len())?;
            }
            let mut skip = false;
            for other in excluded {
                self.tick()?;
                if let (PropertyKey::String(a), PropertyKey::String(b)) = (&key, other) {
                    self.consume_work(a.len().min(b.len()))?;
                }
                if &key == other {
                    skip = true;
                    break;
                }
            }
            if !skip
                && self
                    .own_enumerability(source.as_object().unwrap(), &key)?
                    .unwrap_or(false)
            {
                let value = self.get_property_key(source.clone(), key.clone())?;
                self.define_data_property_key(target.clone(), key, value, true, true, true)?;
            }
        }
        Ok(())
    }
    pub(crate) fn template_object(&mut self, site: Rc<TemplateSite>) -> Result<Value, JsError> {
        let id = Rc::as_ptr(&site) as usize;
        if let Some((_, value)) = self.template_objects.get(&id) {
            return Ok(value.clone());
        }
        self.reserve_allocation(128)?;
        self.ensure_allocation(
            site.parts
                .len()
                .saturating_mul(std::mem::size_of::<Value>())
                .saturating_mul(2),
        )?;
        let raw = site
            .parts
            .iter()
            .map(|part| Value::String(part.raw.clone()))
            .collect::<Vec<_>>();
        let cooked = site
            .parts
            .iter()
            .map(|part| part.cooked.clone().map(Value::String).unwrap_or_default())
            .collect::<Vec<_>>();
        let raw = self.try_new_array(&raw)?;
        self.freeze_syntax_object(raw.clone())?;
        let cooked = self.try_new_array(&cooked)?;
        self.define_data_property(cooked.clone(), "raw", raw, false, false, false)?;
        self.freeze_syntax_object(cooked.clone())?;
        self.template_objects
            .insert(id, (Rc::downgrade(&site), cooked.clone()));
        Ok(cooked)
    }
    fn freeze_syntax_object(&mut self, value: Value) -> Result<(), JsError> {
        let id = value.as_object().unwrap();
        let count = self.heap.object(id)?.properties.len();
        self.consume_work(count)?;
        let object = self.heap.object_mut(id)?;
        for property in object.properties.values_mut() {
            property.configurable = false;
            if let PropertyKind::Data { writable, .. } = &mut property.kind {
                *writable = false;
            }
        }
        if let ObjectKind::Array {
            length_writable, ..
        } = &mut object.kind
        {
            *length_writable = false;
        }
        object.extensible = false;
        Ok(())
    }
}

use crate::{
    JsError, JsString, Value, Vm,
    binary::NumericIndex,
    heap::{ObjectKind, PropertyKind},
};

/// Array-index property names have at most ten decimal digits.
fn index_key(index: u32, buffer: &mut [u16; 10]) -> &[u16] {
    let mut value = index;
    let mut start = buffer.len();
    loop {
        start -= 1;
        buffer[start] = u16::from(b'0') + (value % 10) as u16;
        value /= 10;
        if value == 0 {
            return &buffer[start..];
        }
    }
}

impl Vm {
    pub(super) fn read_indexed_data(
        &mut self,
        receiver: &Value,
        number: f64,
    ) -> Result<Option<Value>, JsError> {
        let index = NumericIndex::from_number(number);
        if let Value::String(text) = receiver {
            if let NumericIndex::Element(i) = index
                && let Some(&unit) = text.as_utf16().get(i)
            {
                self.consume_work(1)?;
                return Ok(Some(Value::String(JsString::from_utf16_slice(&[unit]))));
            }
            return Ok(None);
        }
        let Some(id) = receiver.as_object() else {
            return Ok(None);
        };
        let object = self.heap.object(id)?;
        match object.kind {
            ObjectKind::TypedArray(_) => {
                self.consume_work(1)?;
                Ok(Some(self.typed_index_get(id, index)?))
            }
            ObjectKind::Array { length, .. } => {
                let NumericIndex::Element(i) = index else {
                    return Ok(None);
                };
                if i >= length as usize {
                    return Ok(None);
                }
                let mut digits = [0; 10];
                let key = index_key(i as u32, &mut digits);
                let Some(property) = object.properties.get(key) else {
                    return Ok(None);
                };
                let PropertyKind::Data { value, .. } = &property.kind else {
                    return Ok(None);
                };
                let value = value.clone();
                self.consume_work(1)?;
                Ok(Some(value))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn write_indexed_data(
        &mut self,
        receiver: &Value,
        number: f64,
        value: &Value,
        strict: bool,
    ) -> Result<bool, JsError> {
        let Some(id) = receiver.as_object() else {
            return Ok(false);
        };
        let index = NumericIndex::from_number(number);
        match self.heap.object(id)?.kind {
            ObjectKind::TypedArray(_) => {
                self.consume_work(1)?;
                if self.typed_buffer_immutable(id)? {
                    if strict {
                        return Err(JsError::type_error("ArrayBuffer is immutable"));
                    }
                } else {
                    self.typed_index_set(id, index, value)?;
                }
                Ok(true)
            }
            ObjectKind::Array { length, .. } => {
                let NumericIndex::Element(i) = index else {
                    return Ok(false);
                };
                if i >= length as usize {
                    return Ok(false);
                }
                let mut digits = [0; 10];
                let key = index_key(i as u32, &mut digits);
                self.consume_work(1)?;
                let object = self.heap.object_mut(id)?;
                if let Some(property) = object.properties.get_mut(key)
                    && let PropertyKind::Data {
                        value: old,
                        writable: true,
                    } = &mut property.kind
                {
                    *old = value.clone();
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }
}

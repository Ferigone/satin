use super::{TypedArray, View, element::Element, kind_id, type_error};
use crate::{JsError, Value, Vm, heap::ObjectKind};
use std::ops::Range;

/// The intrinsic kind and element width of an ArrayBufferView.
#[derive(Clone, Copy)]
pub struct ArrayBufferViewKind(Option<Element>);

impl ArrayBufferViewKind {
    pub const UINT8: Self = Self(Some(Element::Uint8));

    pub fn element_width(self) -> usize {
        self.0.map_or(1, Element::width)
    }

    pub fn is_integer_typed_array(self) -> bool {
        matches!(self.0, Some(
            Element::Int8 | Element::Uint8 | Element::Uint8Clamped
            | Element::Int16 | Element::Uint16 | Element::Int32 | Element::Uint32
            | Element::BigInt64 | Element::BigUint64
        ))
    }
}

/// A native ArrayBufferView's underlying buffer and visible byte range.
/// Long-lived owners should keep `buffer` in a traced slot.
#[derive(Clone)]
pub struct ArrayBufferView {
    pub buffer: Value,
    pub byte_range: Range<usize>,
    pub kind: ArrayBufferViewKind,
}

impl Vm {
    fn array_buffer_view_layout(&self, value: &Value) -> Result<(View, ArrayBufferViewKind), JsError> {
        let id = kind_id(self, value, "Expected an ArrayBufferView")?;
        let (view, kind) = match self.heap.object(id)?.kind {
            ObjectKind::TypedArray(array) => (array.view, Some(array.element)),
            ObjectKind::DataView(view) => (view, None),
            _ => return Err(JsError::type_error("Expected an ArrayBufferView")),
        };
        Ok((view, ArrayBufferViewKind(kind)))
    }

    /// Reports the intrinsic view kind, including for a detached view.
    pub fn array_buffer_view_kind(&self, value: &Value) -> Result<ArrayBufferViewKind, JsError> {
        self.array_buffer_view_layout(value).map(|(_, kind)| kind)
    }

    /// Reads a Web IDL ArrayBufferView without consulting its JS properties.
    pub fn fixed_array_buffer_view(&self, value: &Value) -> Result<ArrayBufferView, JsError> {
        let (view, kind) = self.array_buffer_view_layout(value)?;
        if self.buffer_data(view.buffer)?.maximum.is_some() {
            return Err(JsError::type_error(
                "Resizable ArrayBufferView is not allowed",
            ));
        }
        Ok(ArrayBufferView {
            buffer: Value::Object(view.buffer),
            byte_range: self.require_view_range(view, kind.element_width())?,
            kind,
        })
    }

    /// Creates an intrinsic typed array or DataView over the given buffer range.
    pub fn new_array_buffer_view(
        &mut self,
        buffer: &Value,
        range: Range<usize>,
        kind: ArrayBufferViewKind,
    ) -> Result<Value, JsError> {
        let buffer = kind_id(self, buffer, "Expected an ArrayBuffer")?;
        let width = kind.element_width();
        if range.end < range.start
            || !range.start.is_multiple_of(width)
            || !range.len().is_multiple_of(width)
        {
            return Err(type_error());
        }
        let view = View {
            buffer,
            offset: range.start,
            bytes: Some(range.len()),
        };
        self.require_view_range(view, width)?;
        match kind.0 {
            Some(element) => self.new_typed_view(
                TypedArray { view, element },
                self.realm.binary.typed[element as usize]
                    .expect("initialized typed array prototype"),
            ),
            None => {
                self.check_binary_allocation(0, 1)?;
                self.allocate_constructed(
                    ObjectKind::DataView(view),
                    self.realm
                        .binary
                        .data_view
                        .expect("initialized DataView prototype"),
                )
            }
        }
    }

    pub fn new_zeroed_array_buffer(&mut self, length: usize) -> Result<Value, JsError> {
        let bytes = self.zero_bytes(length, 1)?;
        self.new_array_buffer(bytes)
    }

    pub fn array_buffer_byte_length(&self, value: &Value) -> Result<usize, JsError> {
        let id = kind_id(self, value, "Expected an ArrayBuffer")?;
        self.buffer_data(id)?
            .length()
            .ok_or_else(type_error)
    }

    /// Transfers the original backing store into a fixed-length buffer.
    pub fn transfer_array_buffer(&mut self, value: &Value) -> Result<Value, JsError> {
        let id = kind_id(self, value, "Expected an ArrayBuffer")?;
        self.transfer_buffer(id, Value::Undefined, true)
    }

    pub fn copy_array_buffer_range(
        &mut self,
        buffer: &Value,
        range: Range<usize>,
    ) -> Result<Vec<u8>, JsError> {
        let id = kind_id(self, buffer, "Expected an ArrayBuffer")?;
        self.copy_binary_range(id, range)
    }

    pub fn copy_into_array_buffer(
        &mut self,
        source: &Value,
        range: Range<usize>,
        target: &Value,
        offset: usize,
    ) -> Result<(), JsError> {
        if range.end < range.start {
            return Err(type_error());
        }
        let source = kind_id(self, source, "Expected a source ArrayBuffer")?;
        let target = kind_id(self, target, "Expected a target ArrayBuffer")?;
        self.copy_binary_between(source, range, target, offset)
    }

    pub fn write_array_buffer(
        &mut self,
        target: &Value,
        offset: usize,
        source: &[u8],
    ) -> Result<(), JsError> {
        let id = kind_id(self, target, "Expected a target ArrayBuffer")?;
        let end = offset.checked_add(source.len()).ok_or_else(type_error)?;
        self.binary_work(source.len())?;
        let mut bytes = self.buffer_data_mut(id)?.write().ok_or_else(type_error)?;
        let destination = bytes.get_mut(offset..end).ok_or_else(type_error)?;
        destination.copy_from_slice(source);
        Ok(())
    }
}

//! Native backing stores and typed views for ECMAScript and browser host APIs.
mod array_buffer;
mod data_view;
mod element;
mod host_views;
mod shared;
mod structured_clone;
mod typed_array;
mod typed_methods;
mod uint8_codec;

use crate::{
    ErrorKind, JsError, ObjectId, PropertyKey, Value, Vm,
    heap::{ObjectKind, Property, PropertyKind},
};
use element::Element;
pub use host_views::{ArrayBufferView, ArrayBufferViewKind};
use std::{ops::Range, rc::Rc};
pub(crate) use structured_clone::ViewLayout;
pub(crate) use typed_array::{NumericIndex, TypedDescriptor, numeric_index};

pub(crate) use element::f16_round;

const CELL_BYTES: usize = 384;
const MAX_INDEX: f64 = 9_007_199_254_740_991.0;
const MAX_BUFFER_BYTES: usize = u32::MAX as usize;

#[derive(Default)]
pub(crate) struct Intrinsics {
    array_buffer: Option<ObjectId>,
    shared_array_buffer: Option<ObjectId>,
    data_view: Option<ObjectId>,
    typed: [Option<ObjectId>; Element::COUNT],
    typed_constructors: [Option<ObjectId>; Element::COUNT],
}

pub(crate) struct Buffer {
    bytes: Option<Vec<u8>>,
    maximum: Option<usize>,
    wasm: bool,
    immutable: bool,
}

impl Buffer {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.as_ref().map_or(0, Vec::capacity)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct View {
    pub(crate) buffer: ObjectId,
    offset: usize,
    bytes: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct TypedArray {
    pub(crate) view: View,
    element: Element,
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn type_error() -> JsError {
    JsError::type_error("Buffer source is detached or out of bounds")
}

fn range_error(message: &'static str) -> JsError {
    JsError::new(ErrorKind::RangeError, message)
}

fn allocation_error() -> JsError {
    JsError::new(
        ErrorKind::HeapLimit,
        "Binary backing store allocation failed",
    )
}

pub(crate) fn to_index(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    let number = vm.to_number(value)?;
    if number.is_nan() {
        return Ok(0);
    }
    let integer = number.trunc();
    if !(0.0..=MAX_INDEX.min(usize::MAX as f64)).contains(&integer) {
        return Err(range_error("Invalid binary index"));
    }
    Ok(integer as usize)
}

fn to_length(vm: &mut Vm, value: &Value) -> Result<usize, JsError> {
    let number = vm.to_number(value)?;
    Ok(number.trunc().clamp(0.0, MAX_INDEX.min(usize::MAX as f64)) as usize)
}

fn clamped_index(vm: &mut Vm, value: &Value, length: usize) -> Result<usize, JsError> {
    Ok(absolute_index(vm, value, length)?.clamp(0.0, length as f64) as usize)
}

fn absolute_index(vm: &mut Vm, value: &Value, length: usize) -> Result<f64, JsError> {
    let index = vm.to_number(value)?.trunc();
    Ok(if index.is_nan() {
        0.0
    } else if index < 0.0 {
        length as f64 + index
    } else {
        index
    })
}

fn kind_id(vm: &Vm, value: &Value, brand: &'static str) -> Result<ObjectId, JsError> {
    let id = value
        .as_object()
        .ok_or_else(|| JsError::type_error(brand))?;
    vm.heap.object(id)?;
    Ok(id)
}

impl Vm {
    fn check_binary_allocation(&self, bytes: usize, cells: usize) -> Result<(), JsError> {
        if self.heap.live.saturating_add(cells) > self.limits.max_heap_objects {
            return Err(JsError::new(
                ErrorKind::HeapLimit,
                "Binary object budget exhausted",
            ));
        }
        self.ensure_allocation(bytes.saturating_add(cells.saturating_mul(CELL_BYTES)))
    }

    fn binary_work(&mut self, bytes: usize) -> Result<(), JsError> {
        // A block copy or zero fill does not have per-element JS overhead.
        self.consume_work(bytes.div_ceil(64))
    }

    fn backing_id(&self, id: ObjectId) -> Result<ObjectId, JsError> {
        match self.heap.object(id)?.kind {
            ObjectKind::ArrayBuffer(_) => Ok(id),
            ObjectKind::SharedArrayBuffer(backing) => Ok(backing),
            _ => Err(JsError::type_error("Expected a binary buffer")),
        }
    }
    fn require_unshared(&self, id: ObjectId) -> Result<(), JsError> {
        if matches!(self.heap.object(id)?.kind, ObjectKind::ArrayBuffer(_)) {
            Ok(())
        } else {
            Err(JsError::type_error("Expected an unshared ArrayBuffer"))
        }
    }
    fn buffer_data(&self, id: ObjectId) -> Result<&Buffer, JsError> {
        let id = self.backing_id(id)?;
        match &self.heap.object(id)?.kind {
            ObjectKind::ArrayBuffer(data) => Ok(data),
            _ => Err(JsError::type_error("Receiver is not an ArrayBuffer")),
        }
    }

    pub(crate) fn buffer_is_immutable(&self, id: ObjectId) -> Result<bool, JsError> {
        Ok(self.buffer_data(id)?.immutable)
    }
    fn require_writable_buffer(&self, id: ObjectId) -> Result<(), JsError> {
        if self.buffer_is_immutable(id)? {
            return Err(JsError::type_error("ArrayBuffer is immutable"));
        }
        Ok(())
    }
    fn buffer_data_mut(&mut self, id: ObjectId) -> Result<&mut Buffer, JsError> {
        self.require_writable_buffer(id)?;
        let id = self.backing_id(id)?;
        match &mut self.heap.object_mut(id)?.kind {
            ObjectKind::ArrayBuffer(data) => Ok(data),
            _ => Err(JsError::type_error("Receiver is not an ArrayBuffer")),
        }
    }

    fn array_data(&self, id: ObjectId) -> Result<TypedArray, JsError> {
        match self.heap.object(id)?.kind {
            ObjectKind::TypedArray(data) => Ok(data),
            _ => Err(JsError::type_error("Receiver is not a TypedArray")),
        }
    }

    fn array_buffer_prototype(&self) -> ObjectId {
        self.realm
            .binary
            .array_buffer
            .expect("initialized ArrayBuffer prototype")
    }

    fn create_buffer(
        &mut self,
        bytes: Vec<u8>,
        maximum: Option<usize>,
        prototype: ObjectId,
    ) -> Result<Value, JsError> {
        check_buffer_length(bytes.len())?;
        self.check_binary_allocation(bytes.capacity(), 1)?;
        self.allocate_constructed(
            ObjectKind::ArrayBuffer(Buffer {
                bytes: Some(bytes),
                maximum,
                wasm: false,
                immutable: false,
            }),
            prototype,
        )
    }

    fn zero_bytes(&mut self, length: usize, cells: usize) -> Result<Vec<u8>, JsError> {
        check_buffer_length(length)?;
        self.check_binary_allocation(length, cells)?;
        self.binary_work(length)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| allocation_error())?;
        bytes.resize(length, 0);
        Ok(bytes)
    }

    fn new_typed_view(&mut self, data: TypedArray, prototype: ObjectId) -> Result<Value, JsError> {
        self.check_binary_allocation(0, 1)?;
        self.allocate_constructed(ObjectKind::TypedArray(data), prototype)
    }

    fn view_range(&self, view: View, alignment: usize) -> Result<Option<Range<usize>>, JsError> {
        let Some(buffer) = &self.buffer_data(view.buffer)?.bytes else {
            return Ok(None);
        };
        let Some(available) = buffer.len().checked_sub(view.offset) else {
            return Ok(None);
        };
        let length = view.bytes.unwrap_or(available / alignment * alignment);
        Ok((length <= available).then_some(view.offset..view.offset + length))
    }

    fn require_view_range(&self, view: View, alignment: usize) -> Result<Range<usize>, JsError> {
        self.view_range(view, alignment)?.ok_or_else(type_error)
    }

    fn buffer_source_range(
        &self,
        value: &Value,
    ) -> Result<Option<(ObjectId, Range<usize>)>, JsError> {
        let Some(id) = value.as_object() else {
            return Ok(None);
        };
        match &self.heap.object(id)?.kind {
            ObjectKind::ArrayBuffer(data) => {
                let bytes = data.bytes.as_ref().ok_or_else(type_error)?;
                Ok(Some((id, 0..bytes.len())))
            }
            ObjectKind::TypedArray(data) => {
                let range = self.require_view_range(data.view, data.element.width())?;
                Ok(Some((data.view.buffer, range)))
            }
            ObjectKind::DataView(view) => {
                Ok(Some((view.buffer, self.require_view_range(*view, 1)?)))
            }
            _ => Ok(None),
        }
    }

    /// Moves host bytes into a fixed-length native ArrayBuffer.
    pub fn new_array_buffer(&mut self, bytes: Vec<u8>) -> Result<Value, JsError> {
        self.create_buffer(bytes, None, self.array_buffer_prototype())
    }

    pub(crate) fn new_wasm_buffer(&mut self, length: usize) -> Result<ObjectId, JsError> {
        let bytes = self.zero_bytes(length, 2)?;
        let id = self.new_array_buffer(bytes)?.as_object().unwrap();
        self.buffer_data_mut(id)?.wasm = true;
        Ok(id)
    }

    pub(crate) fn grow_wasm_buffer(
        &mut self,
        id: ObjectId,
        length: usize,
    ) -> Result<ObjectId, JsError> {
        let capacity = self.buffer_data(id)?.retained_bytes();
        // Reallocation can temporarily hold both blocks. Preflight the peak,
        // not just the eventual increase in capacity.
        let temporary = if length > capacity { length } else { 0 };
        self.check_binary_allocation(temporary, 1)?;
        let replacement = self.new_array_buffer(Vec::new())?.as_object().unwrap();
        self.resize_binary_store(id, length)?;
        let bytes = self.buffer_data_mut(id)?.bytes.take().unwrap();
        *self.buffer_data_mut(replacement)? = Buffer {
            bytes: Some(bytes),
            maximum: None,
            wasm: true,
            immutable: false,
        };
        Ok(replacement)
    }

    pub(crate) fn wasm_buffer_bytes(&self, id: ObjectId) -> &[u8] {
        self.buffer_data(id)
            .expect("traced WebAssembly buffer")
            .bytes
            .as_deref()
            .expect("current WebAssembly buffer")
    }

    pub(crate) fn wasm_buffer_bytes_mut(&mut self, id: ObjectId) -> &mut [u8] {
        self.buffer_data_mut(id)
            .expect("traced WebAssembly buffer")
            .bytes
            .as_deref_mut()
            .expect("current WebAssembly buffer")
    }

    /// Moves host bytes into a native Uint8Array with its own ArrayBuffer.
    pub fn new_uint8_array(&mut self, bytes: Vec<u8>) -> Result<Value, JsError> {
        self.new_binary_view(bytes, Element::Uint8)
    }

    fn new_binary_view(&mut self, bytes: Vec<u8>, element: Element) -> Result<Value, JsError> {
        self.check_binary_allocation(bytes.capacity(), 2)?;
        let length = bytes.len();
        let buffer = self.new_array_buffer(bytes)?.as_object().unwrap();
        self.new_typed_view(
            TypedArray {
                view: View {
                    buffer,
                    offset: 0,
                    bytes: Some(length),
                },
                element,
            },
            self.realm.binary.typed[element as usize].expect("initialized typed array prototype"),
        )
    }

    /// Preflights native storage before filling a new view's bytes in place.
    pub fn new_uint8_array_with(
        &mut self,
        length: usize,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        self.new_binary_view_with(length, Element::Uint8, fill)
    }

    /// Creates native clamped-byte storage without consulting page globals.
    pub fn new_uint8_clamped_array_with(
        &mut self,
        length: usize,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        self.new_binary_view_with(length, Element::Uint8Clamped, fill)
    }

    /// Creates a native Float16Array. Fill the backing slice in little-endian pairs.
    pub fn new_float16_array_with(
        &mut self,
        length: usize,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        self.new_binary_view_with(length, Element::Float16, fill)
    }

    /// Encodes an element of a native Float16Array backing store.
    pub fn encode_float16(value: f64) -> [u8; 2] {
        let bits = Element::Float16.number_bits(value);
        let bytes = Element::Float16.store_bits(bits, true);
        [bytes[0], bytes[1]]
    }

    /// Decodes an element of a native Float16Array backing store.
    pub fn decode_float16(bytes: [u8; 2]) -> f64 {
        Element::Float16.number_value(Element::load_bits(&bytes, true))
    }

    fn new_binary_view_with(
        &mut self,
        length: usize,
        element: Element,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<Value, JsError> {
        self.with_task(|vm| {
            let byte_length = length
                .checked_mul(element.width())
                .ok_or_else(|| range_error("Typed array length out of range"))?;
            let mut bytes = vm.zero_bytes(byte_length, 2)?;
            fill(&mut bytes);
            vm.new_binary_view(bytes, element)
        })
    }

    /// Checks the native BufferSource brand, including detached views.
    pub fn is_buffer_source(&self, value: &Value) -> bool {
        value
            .as_object()
            .and_then(|id| self.heap.object(id).ok())
            .is_some_and(|object| {
                matches!(
                    &object.kind,
                    ObjectKind::ArrayBuffer(_)
                        | ObjectKind::TypedArray(_)
                        | ObjectKind::DataView(_)
                )
            })
    }

    /// Checks the Uint8Array internal brand independently of its prototype.
    pub fn is_uint8_array(&self, value: &Value) -> bool {
        self.has_typed_array_element(value, Element::Uint8)
    }

    /// Checks the Uint8ClampedArray internal brand, including detached views.
    pub fn is_uint8_clamped_array(&self, value: &Value) -> bool {
        self.has_typed_array_element(value, Element::Uint8Clamped)
    }

    /// Checks the Float16Array internal brand, including detached views.
    pub fn is_float16_array(&self, value: &Value) -> bool {
        self.has_typed_array_element(value, Element::Float16)
    }

    fn has_typed_array_element(&self, value: &Value, element: Element) -> bool {
        value
            .as_object()
            .and_then(|id| self.array_data(id).ok())
            .is_some_and(|data| data.element == element)
    }

    /// Detaches an ArrayBuffer and releases its bytes. A view itself cannot be detached.
    pub fn detach_array_buffer(&mut self, value: &Value) -> Result<(), JsError> {
        let id = kind_id(self, value, "Expected an ArrayBuffer to detach")?;
        self.require_unshared(id)?;
        let buffer = self.buffer_data_mut(id)?;
        if buffer.wasm {
            return Err(JsError::type_error("Cannot detach a WebAssembly buffer"));
        }
        let bytes = buffer.bytes.take();
        let capacity = bytes.as_ref().map_or(0, Vec::capacity);
        self.heap.estimated_bytes = self.heap.estimated_bytes.saturating_sub(capacity);
        Ok(())
    }

    /// Borrows a fixed-length Web IDL BufferSource. Detachment means an empty
    /// input. Callers that require live buffers can use buffer_source_len.
    pub fn fixed_buffer_source_bytes(&self, value: &Value) -> Result<&[u8], JsError> {
        let id = kind_id(self, value, "Expected a BufferSource")?;
        let (buffer, view) = match self.heap.object(id)?.kind {
            ObjectKind::ArrayBuffer(_) => (id, None),
            ObjectKind::TypedArray(array) => (array.view.buffer, Some(array.view)),
            ObjectKind::DataView(view) => (view.buffer, Some(view)),
            _ => return Err(JsError::type_error("Expected a BufferSource")),
        };
        self.require_unshared(buffer)?;
        let data = self.buffer_data(buffer)?;
        if data.maximum.is_some() {
            return Err(JsError::type_error("Resizable BufferSource is not allowed"));
        }
        let Some(bytes) = &data.bytes else {
            return Ok(&[]);
        };
        if let Some(view) = view {
            let range = self.require_view_range(view, 1)?;
            Ok(&bytes[range])
        } else {
            Ok(bytes)
        }
    }

    /// Borrows the writable extent of a fixed-length native Uint8Array.
    /// An empty slice is returned after detachment.
    pub fn fixed_uint8_array_bytes_mut(&mut self, value: &Value) -> Result<&mut [u8], JsError> {
        let id = kind_id(self, value, "Expected a Uint8Array")?;
        let array = self.array_data(id)?;
        if array.element != Element::Uint8 {
            return Err(JsError::type_error("Expected a Uint8Array"));
        }
        self.require_unshared(array.view.buffer)?;
        if self.buffer_data(array.view.buffer)?.maximum.is_some() {
            return Err(JsError::type_error("Resizable Uint8Array is not allowed"));
        }
        let Some(range) = self.view_range(array.view, 1)? else {
            return Ok(&mut []);
        };
        Ok(&mut self
            .buffer_data_mut(array.view.buffer)?
            .bytes
            .as_mut()
            .unwrap()[range])
    }

    /// Returns the size of a view's visible bytes. Detached and out-of-bounds
    /// views are invalid sources. Other values have no buffer extent.
    pub fn buffer_source_len(&self, value: &Value) -> Result<Option<usize>, JsError> {
        Ok(self
            .buffer_source_range(value)?
            .map(|(_, range)| range.len()))
    }

    /// Takes an independent byte snapshot of the visible extent. A host can
    /// reserve its own destination quota first using buffer_source_len.
    pub fn copy_buffer_source(&mut self, value: &Value) -> Result<Option<Vec<u8>>, JsError> {
        self.with_task(|vm| {
            let Some((buffer, range)) = vm.buffer_source_range(value)? else {
                return Ok(None);
            };
            vm.copy_binary_range(buffer, range).map(Some)
        })
    }

    fn copy_binary_range(&mut self, id: ObjectId, range: Range<usize>) -> Result<Vec<u8>, JsError> {
        self.ensure_allocation(range.len())?;
        self.binary_work(range.len())?;
        let bytes = self
            .buffer_data(id)?
            .bytes
            .as_ref()
            .ok_or_else(type_error)?;
        let source = bytes.get(range).ok_or_else(type_error)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(source.len())
            .map_err(|_| allocation_error())?;
        output.extend_from_slice(source);
        Ok(output)
    }

    fn copy_binary_between(
        &mut self,
        source: ObjectId,
        range: Range<usize>,
        target: ObjectId,
        offset: usize,
    ) -> Result<(), JsError> {
        self.require_writable_buffer(target)?;
        let source = self.backing_id(source)?;
        let target = self.backing_id(target)?;
        self.binary_work(range.len())?;
        let end = offset.checked_add(range.len()).ok_or_else(type_error)?;
        if source == target {
            let buffer = self
                .buffer_data_mut(source)?
                .bytes
                .as_mut()
                .ok_or_else(type_error)?;
            if end > buffer.len() || range.end > buffer.len() {
                return Err(type_error());
            }
            buffer.copy_within(range, offset);
        } else {
            let (source, target) = self.heap.distinct_objects_mut(source, target)?;
            let (ObjectKind::ArrayBuffer(source), ObjectKind::ArrayBuffer(target)) =
                (&source.kind, &mut target.kind)
            else {
                return Err(JsError::type_error("Expected two ArrayBuffers"));
            };
            let data = source
                .bytes
                .as_ref()
                .and_then(|bytes| bytes.get(range))
                .ok_or_else(type_error)?;
            let destination = target
                .bytes
                .as_mut()
                .and_then(|bytes| bytes.get_mut(offset..end))
                .ok_or_else(type_error)?;
            destination.copy_from_slice(data);
        }
        Ok(())
    }
}

fn check_buffer_length(length: usize) -> Result<(), JsError> {
    if length > MAX_BUFFER_BYTES {
        Err(range_error(
            "ArrayBuffer length exceeds the implementation limit",
        ))
    } else {
        Ok(())
    }
}

fn getter(
    vm: &mut Vm,
    object: Value,
    key: PropertyKey,
    name: &str,
    callback: impl Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static,
) {
    let read = vm.native(name, 0, false, Rc::new(callback));
    vm.define_key(
        object,
        key,
        Property {
            kind: PropertyKind::Accessor {
                get: Some(read),
                set: None,
            },
            enumerable: false,
            configurable: true,
        },
    )
    .unwrap();
}

fn species_getter(vm: &mut Vm, constructor: Value) {
    let key = vm.well_known_key("species");
    getter(
        vm,
        constructor,
        key,
        "get [Symbol.species]",
        |_, this, _| Ok(this),
    );
}

fn species_constructor(vm: &mut Vm, object: Value, fallback: ObjectId) -> Result<Value, JsError> {
    let constructor = vm.get_property(object, "constructor")?;
    if constructor.is_undefined() {
        return Ok(Value::Object(fallback));
    }
    if constructor.as_object().is_none() {
        return Err(JsError::type_error("Invalid buffer constructor"));
    }
    let species = vm.get_property_key(constructor, vm.well_known_key("species"))?;
    if species.is_nullish() {
        Ok(Value::Object(fallback))
    } else if vm.is_constructor(&species) {
        Ok(species)
    } else {
        Err(JsError::type_error("Binary species is not a constructor"))
    }
}

fn tag(vm: &mut Vm, object: Value, text: &str) {
    vm.define_data_property_key(
        object,
        vm.well_known_key("toStringTag"),
        text.into(),
        false,
        false,
        true,
    )
    .unwrap();
}

pub(crate) fn install(vm: &mut Vm) {
    array_buffer::install(vm);
    typed_array::install(vm);
    uint8_codec::install(vm);
    data_view::install(vm);
    shared::install(vm);
}

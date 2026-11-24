use super::{
    TypedArray, View, arg, element::Element, getter, kind_id, range_error, species_getter,
    to_index, to_length, type_error,
};
use crate::{
    JsError, JsString, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind, array_index},
    iterator::ArrayKind,
    vm::number_string,
};
use std::rc::Rc;

pub(super) const LITTLE_ENDIAN: bool = cfg!(target_endian = "little");

#[derive(Clone, Copy)]
pub(crate) enum NumericIndex {
    Element(usize),
    Invalid,
}

pub(crate) struct TypedDescriptor {
    value: Option<Value>,
    allowed: bool,
}

impl TypedDescriptor {
    pub(crate) fn read(vm: &mut Vm, object: Value) -> Result<Self, JsError> {
        let mut member = |name: &str| -> Result<Option<Value>, JsError> {
            if vm.has(&object, &name.into())? {
                Ok(Some(vm.get_property(object.clone(), name)?))
            } else {
                Ok(None)
            }
        };
        let enumerable = member("enumerable")?;
        let configurable = member("configurable")?;
        let value = member("value")?;
        let writable = member("writable")?;
        // Check each accessor immediately, before observing the next field.
        let get = member("get")?;
        if get
            .as_ref()
            .is_some_and(|function| !function.is_undefined() && !vm.is_callable(function))
        {
            return Err(JsError::type_error("Getter must be callable or undefined"));
        }
        let set = if vm.has(&object, &"set".into())? {
            Some(vm.get_property(object, "set")?)
        } else {
            None
        };
        if set
            .as_ref()
            .is_some_and(|function| !function.is_undefined() && !vm.is_callable(function))
        {
            return Err(JsError::type_error("Setter must be callable or undefined"));
        }
        let accessor = get.is_some() || set.is_some();
        if accessor && (value.is_some() || writable.is_some()) {
            return Err(JsError::type_error(
                "Descriptor cannot be both data and accessor",
            ));
        }
        Ok(Self {
            value,
            allowed: !accessor
                && [enumerable, configurable, writable]
                    .into_iter()
                    .all(|flag| flag.is_none_or(|value| value.is_truthy())),
        })
    }
}

pub(crate) fn numeric_index(key: &JsString) -> Option<NumericIndex> {
    if let Some(index) = array_index(key) {
        return Some(NumericIndex::Element(index as usize));
    }
    // A canonical Number string uses only a short sequence of ASCII code units.
    let units = key.as_utf16();
    if units.is_empty() || units.len() > 32 {
        return None;
    }
    let mut ascii = [0; 32];
    for (byte, unit) in ascii.iter_mut().zip(units) {
        if *unit > 127 {
            return None;
        }
        *byte = *unit as u8;
    }
    let text = std::str::from_utf8(&ascii[..units.len()]).unwrap();
    if text == "-0" {
        return Some(NumericIndex::Invalid);
    }
    let number = text.parse::<f64>().ok()?;
    if number_string(number) != text {
        return None;
    }
    if number >= 0.0 && number < usize::MAX as f64 && number.fract() == 0.0 {
        Some(NumericIndex::Element(number as usize))
    } else {
        Some(NumericIndex::Invalid)
    }
}

impl Vm {
    pub(super) fn allocate_typed_array(
        &mut self,
        element: Element,
        length: usize,
        prototype: ObjectId,
    ) -> Result<Value, JsError> {
        let count = byte_count(length, element)?;
        let bytes = self.zero_bytes(count, 2)?;
        let buffer = self.new_array_buffer(bytes)?.as_object().unwrap();
        self.new_typed_view(
            TypedArray {
                view: View {
                    buffer,
                    offset: 0,
                    bytes: Some(count),
                },
                element,
            },
            prototype,
        )
    }

    pub(crate) fn typed_array_length(&self, id: ObjectId) -> Result<Option<usize>, JsError> {
        let ObjectKind::TypedArray(array) = self.heap.object(id)?.kind else {
            return Ok(None);
        };
        Ok(Some(
            self.require_view_range(array.view, array.element.width())?
                .len()
                / array.element.width(),
        ))
    }

    pub(crate) fn typed_array_key_count(&self, id: ObjectId) -> Result<usize, JsError> {
        let ObjectKind::TypedArray(array) = self.heap.object(id)?.kind else {
            return Ok(0);
        };
        let width = array.element.width();
        Ok(self
            .view_range(array.view, width)?
            .map_or(0, |range| range.len() / width))
    }

    pub(crate) fn typed_can_prevent_extensions(&self, id: ObjectId) -> Result<bool, JsError> {
        let ObjectKind::TypedArray(array) = self.heap.object(id)?.kind else {
            return Ok(true);
        };
        Ok(array.view.bytes.is_some() && self.buffer_data(array.view.buffer)?.maximum.is_none())
    }

    pub(crate) fn typed_element_exists(
        &self,
        id: ObjectId,
        index: NumericIndex,
    ) -> Result<bool, JsError> {
        let NumericIndex::Element(index) = index else {
            return Ok(false);
        };
        Ok(index < self.typed_array_key_count(id)?)
    }

    pub(crate) fn typed_index_get(
        &self,
        id: ObjectId,
        index: NumericIndex,
    ) -> Result<Value, JsError> {
        let NumericIndex::Element(index) = index else {
            return Ok(Value::Undefined);
        };
        self.read_typed_element(self.array_data(id)?, index)
    }

    pub(crate) fn typed_index_set(
        &mut self,
        id: ObjectId,
        index: NumericIndex,
        value: &Value,
    ) -> Result<(), JsError> {
        // Coercion occurs even for invalid indices. It may grow, shrink or detach the buffer.
        let array = self.array_data(id)?;
        let bits = array.element.coerce_bits(self, value)?;
        if let NumericIndex::Element(index) = index {
            self.write_typed_bits(array, index, bits)?;
        }
        Ok(())
    }

    pub(crate) fn define_typed_index(
        &mut self,
        id: ObjectId,
        index: NumericIndex,
        property: &Property,
    ) -> Result<(), JsError> {
        if !self.typed_element_exists(id, index)? || !property.configurable || !property.enumerable
        {
            return Err(JsError::type_error(
                "Invalid TypedArray property definition",
            ));
        }
        let PropertyKind::Data {
            value,
            writable: true,
        } = &property.kind
        else {
            return Err(JsError::type_error(
                "Invalid TypedArray property definition",
            ));
        };
        self.typed_index_set(id, index, value)
    }

    pub(crate) fn apply_typed_descriptor(
        &mut self,
        id: ObjectId,
        index: NumericIndex,
        descriptor: TypedDescriptor,
    ) -> Result<bool, JsError> {
        if !descriptor.allowed || !self.typed_element_exists(id, index)? {
            return Ok(false);
        }
        if let Some(value) = descriptor.value {
            self.typed_index_set(id, index, &value)?;
        }
        Ok(true)
    }

    pub(super) fn read_typed_element(
        &self,
        array: TypedArray,
        index: usize,
    ) -> Result<Value, JsError> {
        Ok(self
            .read_typed_bits(array, index)?
            .map_or(Value::Undefined, |bits| array.element.value(bits)))
    }

    pub(super) fn read_typed_bits(
        &self,
        array: TypedArray,
        index: usize,
    ) -> Result<Option<u64>, JsError> {
        let width = array.element.width();
        let Some(range) = self.view_range(array.view, width)? else {
            return Ok(None);
        };
        if index >= range.len() / width {
            return Ok(None);
        }
        let offset = range.start + index * width;
        let data =
            &self.buffer_data(array.view.buffer)?.bytes.as_ref().unwrap()[offset..offset + width];
        Ok(Some(Element::load_bits(data, LITTLE_ENDIAN)))
    }

    pub(super) fn write_typed_value(
        &mut self,
        array: TypedArray,
        index: usize,
        value: &Value,
    ) -> Result<(), JsError> {
        let bits = array.element.coerce_bits(self, value)?;
        self.write_typed_bits(array, index, bits)
    }

    pub(super) fn write_typed_bits(
        &mut self,
        array: TypedArray,
        index: usize,
        bits: u64,
    ) -> Result<(), JsError> {
        let width = array.element.width();
        let Some(range) = self.view_range(array.view, width)? else {
            return Ok(());
        };
        if index < range.len() / width {
            let offset = range.start + index * width;
            let data = array.element.store_bits(bits, LITTLE_ENDIAN);
            self.buffer_data_mut(array.view.buffer)?
                .bytes
                .as_mut()
                .unwrap()[offset..offset + width]
                .copy_from_slice(&data[..width]);
        }
        Ok(())
    }
}

pub(super) fn byte_count(length: usize, element: Element) -> Result<usize, JsError> {
    length
        .checked_mul(element.width())
        .ok_or_else(|| range_error("TypedArray size overflows"))
}

fn from_buffer(
    vm: &mut Vm,
    buffer: ObjectId,
    args: &[Value],
    element: Element,
    prototype: ObjectId,
) -> Result<Value, JsError> {
    let width = element.width();
    let offset = to_index(vm, &arg(args, 1))?;
    if !offset.is_multiple_of(width) {
        return Err(range_error("TypedArray offset is not aligned"));
    }
    let length = arg(args, 2);
    // The optional length conversion can resize or detach the source buffer.
    let explicit = if length.is_undefined() {
        None
    } else {
        Some(byte_count(to_index(vm, &length)?, element)?)
    };
    let data = vm.buffer_data(buffer)?;
    let len = data.bytes.as_ref().ok_or_else(type_error)?.len();
    let available = len
        .checked_sub(offset)
        .ok_or_else(|| range_error("TypedArray offset exceeds buffer length"))?;
    let bytes = if let Some(bytes) = explicit {
        if bytes > available {
            return Err(range_error("TypedArray length exceeds buffer length"));
        }
        Some(bytes)
    } else if data.maximum.is_some() {
        None
    } else {
        if !len.is_multiple_of(width) {
            return Err(range_error("Buffer byte length is not aligned"));
        }
        Some(available)
    };
    vm.new_typed_view(
        TypedArray {
            view: View {
                buffer,
                offset,
                bytes,
            },
            element,
        },
        prototype,
    )
}

fn create(
    vm: &mut Vm,
    prototype: ObjectId,
    element: Element,
    args: &[Value],
) -> Result<Value, JsError> {
    let source = arg(args, 0);
    let Some(id) = source.as_object() else {
        let length = to_index(vm, &source)?;
        return vm.allocate_typed_array(element, length, prototype);
    };
    match vm.heap.object(id)?.kind {
        ObjectKind::ArrayBuffer(_) => return from_buffer(vm, id, args, element, prototype),
        ObjectKind::TypedArray(array) => {
            let range = vm.require_view_range(array.view, array.element.width())?;
            element.check_content_type(array.element)?;
            let length = range.len() / array.element.width();
            let result = vm.allocate_typed_array(element, length, prototype)?;
            let destination = vm.array_data(result.as_object().unwrap())?;
            if element.same_encoding(array.element) {
                vm.copy_binary_between(array.view.buffer, range, destination.view.buffer, 0)?;
            } else {
                vm.consume_work(length)?;
                for index in 0..length {
                    let bits = vm.read_typed_bits(array, index)?.unwrap();
                    let number = array.element.number_value(bits);
                    vm.write_typed_bits(destination, index, element.number_bits(number))?;
                }
            }
            return Ok(result);
        }
        _ => {}
    }
    let method = vm.get_property_key(source.clone(), vm.well_known_key("iterator"))?;
    if !method.is_nullish() {
        let iterator = vm.iterator_from_method(source, method)?;
        let values = vm.iterator_record_to_list(iterator)?;
        let result = vm.allocate_typed_array(element, values.len(), prototype)?;
        let target = vm.array_data(result.as_object().unwrap())?;
        vm.consume_work(values.len())?;
        for (index, value) in values.iter().enumerate() {
            vm.write_typed_value(target, index, value)?;
        }
        return Ok(result);
    }
    let length = vm.get_property(source.clone(), "length")?;
    let length = to_length(vm, &length)?;
    let result = vm.allocate_typed_array(element, length, prototype)?;
    let target = vm.array_data(result.as_object().unwrap())?;
    for index in 0..length {
        vm.tick()?;
        let value = vm.get_property(source.clone(), &index.to_string())?;
        vm.write_typed_value(target, index, &value)?;
    }
    Ok(result)
}

pub(super) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    vm.root(prototype.clone());
    let abstract_constructor = vm.native(
        "TypedArray",
        0,
        true,
        Rc::new(|_, _, _| Err(JsError::type_error("TypedArray is an abstract constructor"))),
    );
    replace_construct(vm, abstract_constructor.clone(), |_, _, _, _| {
        Err(JsError::type_error("TypedArray is an abstract constructor"))
    });
    vm.root(abstract_constructor.clone());
    vm.define_data_property(
        abstract_constructor.clone(),
        "prototype",
        prototype.clone(),
        false,
        false,
        false,
    )
    .unwrap();
    vm.define_data_property(
        prototype.clone(),
        "constructor",
        abstract_constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    species_getter(vm, abstract_constructor.clone());
    getter(
        vm,
        prototype.clone(),
        "buffer".into(),
        "get buffer",
        |vm, this, _| {
            let id = kind_id(vm, &this, "TypedArray receiver must be an object")?;
            Ok(Value::Object(vm.array_data(id)?.view.buffer))
        },
    );
    for (key, name) in [
        ("length", "get length"),
        ("byteLength", "get byteLength"),
        ("byteOffset", "get byteOffset"),
    ] {
        getter(
            vm,
            prototype.clone(),
            key.into(),
            name,
            move |vm, this, _| {
                let id = kind_id(vm, &this, "TypedArray receiver must be an object")?;
                let data = vm.array_data(id)?;
                let width = data.element.width();
                let range = vm.view_range(data.view, width)?;
                Ok(range
                    .map_or(0, |range| match key {
                        "length" => range.len() / width,
                        "byteLength" => range.len(),
                        _ => range.start,
                    })
                    .into())
            },
        );
    }
    let tag_key = vm.well_known_key("toStringTag");
    getter(
        vm,
        prototype.clone(),
        tag_key,
        "get [Symbol.toStringTag]",
        |vm, this, _| {
            Ok(this
                .as_object()
                .and_then(|id| vm.array_data(id).ok())
                .map_or(Value::Undefined, |data| data.element.name().into()))
        },
    );
    for (name, kind) in [
        ("keys", ArrayKind::Keys),
        ("values", ArrayKind::Values),
        ("entries", ArrayKind::Entries),
    ] {
        method(vm, prototype.clone(), name, 0, move |vm, this, _| {
            let id = kind_id(vm, &this, "TypedArray receiver must be an object")?;
            if vm.typed_array_length(id)?.is_none() {
                return Err(JsError::type_error("Receiver is not a TypedArray"));
            }
            vm.array_iterator(this, kind)
        });
    }
    let values = vm.get_property(prototype.clone(), "values").unwrap();
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("iterator"),
        values,
        true,
        false,
        true,
    )
    .unwrap();
    super::typed_methods::install(vm, prototype.clone(), abstract_constructor.clone());
    for element in Element::ALL {
        let concrete = vm.new_object_with_prototype(prototype.clone()).unwrap();
        let intrinsic = concrete.as_object().unwrap();
        let ctor = constructor(vm, element.name(), 3, concrete.clone(), |_, _, _| {
            Err(JsError::type_error("TypedArray constructor requires new"))
        });
        vm.set_prototype(ctor.as_object().unwrap(), abstract_constructor.as_object())
            .unwrap();
        replace_construct(vm, ctor.clone(), move |vm, _, args, target| {
            let inherited = vm.prototype_from_constructor(target, intrinsic)?;
            create(vm, inherited, element, args)
        });
        for object in [concrete, ctor] {
            vm.root(object.clone());
            vm.define_property(
                object,
                "BYTES_PER_ELEMENT".into(),
                Property {
                    kind: PropertyKind::Data {
                        value: element.width().into(),
                        writable: false,
                    },
                    enumerable: false,
                    configurable: false,
                },
            )
            .unwrap();
        }
        vm.realm.binary.typed[element as usize] = Some(intrinsic);
        vm.realm.binary.typed_constructors[element as usize] = Some(
            vm.get_property(Value::Object(intrinsic), "constructor")
                .unwrap()
                .as_object()
                .unwrap(),
        );
    }
}

//! Shared synchronous iterator protocol. Records are local to active VM execution;
//! iterator objects/captured targets are ordinary traced heap records.
use crate::{
    ErrorKind, JsError, JsString, Value, Vm,
    builtins::method,
    heap::{Cell, Object, ObjectKind, TraceEdges},
};
use std::rc::Rc;
#[derive(Clone, Copy)]
pub(crate) enum ArrayKind {
    Keys,
    Values,
    Entries,
}
#[derive(Clone)]
pub(crate) enum IteratorData {
    Array {
        target: Value,
        index: usize,
        kind: ArrayKind,
    },
    String {
        string: JsString,
        index: usize,
    },
    Done {
        string_kind: bool,
    },
}
impl IteratorData {
    pub(crate) fn trace(&self, out: &mut TraceEdges) {
        if let Self::Array { target, .. } = self {
            out.value(target);
        }
    }
    pub(crate) fn strings(&self, out: &mut Vec<JsString>) {
        if let Self::String { string, .. } = self {
            out.push(string.clone());
        }
    }
}
/// A synchronous iterator and its cached next method. Hosts must use the
/// record within one VM task so that collection cannot invalidate its handles.
pub struct IteratorRecord {
    pub(crate) iterator: Value,
    pub(crate) next: Value,
    pub(crate) done: bool,
}
impl Vm {
    /// The realm's shared iterator prototype, used by native host iterators.
    pub fn intrinsic_iterator_prototype(&self) -> Value {
        Value::Object(
            self.iterator_prototype
                .expect("initialized iterator prototype"),
        )
    }

    pub(crate) fn get_iterator(&mut self, value: Value) -> Result<IteratorRecord, JsError> {
        let method = self.get_property_key(value.clone(), self.well_known_key("iterator"))?;
        self.iterator_from_method(value, method)
    }
    pub fn iterator_from_method(
        &mut self,
        value: Value,
        method: Value,
    ) -> Result<IteratorRecord, JsError> {
        if !self.is_callable(&method) {
            return Err(JsError::type_error("Value is not iterable"));
        }
        let iterator = self.invoke(method, value, &[])?;
        if iterator.as_object().is_none() {
            return Err(JsError::type_error(
                "Iterator method did not return an object",
            ));
        }
        let next = self.get_property(iterator.clone(), "next")?;
        Ok(IteratorRecord {
            iterator,
            next,
            done: false,
        })
    }
    pub fn iterator_step(&mut self, record: &mut IteratorRecord) -> Result<Option<Value>, JsError> {
        self.iterator_step_kind(record, true)
    }
    pub(crate) fn iterator_skip(&mut self, record: &mut IteratorRecord) -> Result<(), JsError> {
        self.iterator_step_kind(record, false).map(|_| ())
    }
    fn iterator_step_kind(
        &mut self,
        record: &mut IteratorRecord,
        read_value: bool,
    ) -> Result<Option<Value>, JsError> {
        if record.done {
            return Ok(None);
        }
        let result = (|| {
            self.tick()?;
            let result = self.invoke(record.next.clone(), record.iterator.clone(), &[])?;
            if result.as_object().is_none() {
                return Err(JsError::type_error("Iterator.next result is not an object"));
            }
            if self.get_property(result.clone(), "done")?.is_truthy() {
                return Ok(None);
            }
            if read_value {
                self.get_property(result, "value").map(Some)
            } else {
                Ok(Some(Value::Undefined))
            }
        })();
        if !matches!(&result, Ok(Some(_))) {
            record.done = true;
        }
        result
    }
    /// A throwing completion wins over ordinary return-getter/call/result errors.
    pub fn iterator_close(
        &mut self,
        record: &mut IteratorRecord,
        completion: Result<Value, JsError>,
    ) -> Result<Value, JsError> {
        if record.done || completion.as_ref().is_err_and(JsError::uncatchable) {
            return completion;
        }
        record.done = true;
        let close = (|| {
            let method = self.get_property(record.iterator.clone(), "return")?;
            if method.is_nullish() {
                return Ok(());
            }
            if !self.is_callable(&method) {
                return Err(JsError::type_error("Iterator.return is not callable"));
            }
            let result = self.invoke(method, record.iterator.clone(), &[])?;
            if result.as_object().is_none() {
                return Err(JsError::type_error(
                    "Iterator.return result is not an object",
                ));
            }
            Ok(())
        })();
        if let Err(error) = &close
            && error.uncatchable()
        {
            return close.map(|_| Value::Undefined);
        }
        match completion {
            Err(error) => Err(error),
            Ok(value) => close.map(|_| value),
        }
    }
    pub(crate) fn iterator_to_list(&mut self, value: Value) -> Result<Vec<Value>, JsError> {
        let iterator = self.get_iterator(value)?;
        self.iterator_record_to_list(iterator)
    }

    pub(crate) fn iterator_record_to_list(
        &mut self,
        mut iterator: IteratorRecord,
    ) -> Result<Vec<Value>, JsError> {
        let mut values = Vec::new();
        while let Some(value) = self.iterator_step(&mut iterator)? {
            if values.len() >= self.limits.max_array_length as usize {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Iterable length budget exhausted",
                ));
            }
            self.reserve_allocation(std::mem::size_of::<Value>() * 2)?;
            values.push(value);
        }
        Ok(values)
    }
    pub fn iterator_result(&mut self, value: Value, done: bool) -> Result<Value, JsError> {
        self.ensure_allocation(1024)?;
        let object = self.new_object();
        self.define_data_property(object.clone(), "value", value, true, true, true)?;
        self.define_data_property(object.clone(), "done", Value::Bool(done), true, true, true)?;
        Ok(object)
    }
    pub(crate) fn array_iterator(
        &mut self,
        value: Value,
        kind: ArrayKind,
    ) -> Result<Value, JsError> {
        let target = self.coerce_object(value)?;
        self.ensure_allocation(256)?;
        Ok(Value::Object(self.heap.allocate(Cell::Object(
            Object::new(
                ObjectKind::Iterator(IteratorData::Array {
                    target,
                    index: 0,
                    kind,
                }),
                self.array_iterator_prototype,
            ),
        ))))
    }
    fn string_iterator(&mut self, value: Value) -> Result<Value, JsError> {
        if value.is_nullish() {
            return Err(JsError::type_error(
                "String iterator receiver is null or undefined",
            ));
        }
        let string = self.to_string(&value)?;
        self.ensure_allocation(256)?;
        Ok(Value::Object(self.heap.allocate(Cell::Object(
            Object::new(
                ObjectKind::Iterator(IteratorData::String { string, index: 0 }),
                self.string_iterator_prototype,
            ),
        ))))
    }
    fn iterator_next_native(&mut self, this: Value, string_kind: bool) -> Result<Value, JsError> {
        let id = this
            .as_object()
            .ok_or_else(|| JsError::type_error("Illegal iterator receiver"))?;
        let ObjectKind::Iterator(data) = &self.heap.object(id)?.kind else {
            return Err(JsError::type_error("Illegal iterator receiver"));
        };
        let data = data.clone();
        match data {
            IteratorData::Done { string_kind: brand } if brand == string_kind => {
                self.iterator_result(Value::Undefined, true)
            }
            IteratorData::String { string, index } if string_kind => {
                if index >= string.len() {
                    self.heap.object_mut(id)?.kind =
                        ObjectKind::Iterator(IteratorData::Done { string_kind: true });
                    return self.iterator_result(Value::Undefined, true);
                }
                let units = string.as_utf16();
                let count = if (0xd800..=0xdbff).contains(&units[index])
                    && units
                        .get(index + 1)
                        .is_some_and(|u| (0xdc00..=0xdfff).contains(u))
                {
                    2
                } else {
                    1
                };
                self.heap.object_mut(id)?.kind = ObjectKind::Iterator(IteratorData::String {
                    string: string.clone(),
                    index: index + count,
                });
                let value = self.copy_string(&units[index..index + count])?;
                self.iterator_result(value, false)
            }
            IteratorData::Array {
                target,
                index,
                kind,
            } if !string_kind => {
                let typed = self.typed_array_length(target.as_object().unwrap())?;
                let length = if let Some(length) = typed {
                    length
                } else {
                    let length = self.get_property(target.clone(), "length")?;
                    let length = self.to_number(&length)?.max(0.0).floor();
                    if length > self.limits.max_array_length as f64 {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "Array iterator length budget exhausted",
                        ));
                    }
                    length as usize
                };
                if index >= length {
                    self.heap.object_mut(id)?.kind =
                        ObjectKind::Iterator(IteratorData::Done { string_kind: false });
                    return self.iterator_result(Value::Undefined, true);
                }
                self.heap.object_mut(id)?.kind = ObjectKind::Iterator(IteratorData::Array {
                    target: target.clone(),
                    index: index + 1,
                    kind,
                });
                let value = match kind {
                    ArrayKind::Keys => Value::from(index),
                    ArrayKind::Values => self.get_property(target, &index.to_string())?,
                    ArrayKind::Entries => {
                        let value = self.get_property(target, &index.to_string())?;
                        self.try_new_array(&[Value::from(index), value])?
                    }
                };
                self.iterator_result(value, false)
            }
            _ => Err(JsError::type_error("Illegal iterator receiver")),
        }
    }
}
pub(crate) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    vm.iterator_prototype = prototype.as_object();
    let identity = vm.native(
        "[Symbol.iterator]",
        0,
        false,
        Rc::new(|_, this, _| Ok(this)),
    );
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("iterator"),
        identity,
        true,
        false,
        true,
    )
    .unwrap();
    let array = vm.new_object_with_prototype(prototype.clone()).unwrap();
    let string = vm.new_object_with_prototype(prototype).unwrap();
    vm.array_iterator_prototype = array.as_object();
    vm.string_iterator_prototype = string.as_object();
    method(vm, array.clone(), "next", 0, |vm, this, _| {
        vm.iterator_next_native(this, false)
    });
    method(vm, string.clone(), "next", 0, |vm, this, _| {
        vm.iterator_next_native(this, true)
    });
    vm.define_data_property_key(
        array,
        vm.well_known_key("toStringTag"),
        Value::from("Array Iterator"),
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property_key(
        string,
        vm.well_known_key("toStringTag"),
        Value::from("String Iterator"),
        false,
        false,
        true,
    )
    .unwrap();
    for (name, kind) in [
        ("keys", ArrayKind::Keys),
        ("values", ArrayKind::Values),
        ("entries", ArrayKind::Entries),
    ] {
        let function = method(
            vm,
            Value::Object(vm.array_prototype),
            name,
            0,
            move |vm, this, _| vm.array_iterator(this, kind),
        );
        if name == "values" {
            vm.define_data_property_key(
                Value::Object(vm.array_prototype),
                vm.well_known_key("iterator"),
                function,
                true,
                false,
                true,
            )
            .unwrap();
        }
    }
    let iter = vm.native(
        "[Symbol.iterator]",
        0,
        false,
        Rc::new(|vm, this, _| vm.string_iterator(this)),
    );
    vm.define_data_property_key(
        Value::Object(vm.string_prototype),
        vm.well_known_key("iterator"),
        iter,
        true,
        false,
        true,
    )
    .unwrap();
    let array = vm.get_global("Array").unwrap();
    method(vm, array, "from", 1, |vm, this, args| {
        let input = args.first().cloned().unwrap_or_default();
        let mapper = args.get(1).cloned().unwrap_or_default();
        let mapping = !mapper.is_undefined();
        if mapping && !vm.is_callable(&mapper) {
            return Err(JsError::type_error("Array.from mapper is not callable"));
        }
        let method = vm.get_property_key(input.clone(), vm.well_known_key("iterator"))?;
        if !method.is_nullish() {
            if !vm.is_callable(&method) {
                return Err(JsError::type_error("Iterator method is not callable"));
            }
            let result = if vm.is_constructor(&this) {
                vm.construct(this, &[])?
            } else {
                vm.try_new_array(&[])?
            };
            let mut record = vm.iterator_from_method(input, method)?;
            let mut index = 0;
            while let Some(value) = vm.iterator_step(&mut record)? {
                let step = (|| {
                    if index >= vm.limits.max_array_length as usize {
                        return Err(JsError::new(
                            ErrorKind::ExecutionLimit,
                            "Array.from length budget exhausted",
                        ));
                    }
                    let value = if mapping {
                        vm.invoke(
                            mapper.clone(),
                            args.get(2).cloned().unwrap_or_default(),
                            &[value, Value::from(index)],
                        )?
                    } else {
                        value
                    };
                    vm.define_data_property(
                        result.clone(),
                        &index.to_string(),
                        value,
                        true,
                        true,
                        true,
                    )?;
                    Ok(Value::Undefined)
                })();
                if let Err(error) = step {
                    return vm.iterator_close(&mut record, Err(error));
                }
                index += 1;
            }
            vm.set_property(result.clone(), "length", Value::from(index))?;
            Ok(result)
        } else {
            let input = vm.coerce_object(input)?;
            let length = vm.get_property(input.clone(), "length")?;
            let length = vm.to_number(&length)?.max(0.0).floor();
            if length > vm.limits.max_array_length as f64 {
                return Err(JsError::new(
                    ErrorKind::ExecutionLimit,
                    "Array.from length budget exhausted",
                ));
            }
            let length = length as usize;
            let result = if vm.is_constructor(&this) {
                vm.construct(this, &[Value::from(length)])?
            } else {
                vm.try_new_array(&[])?
            };
            for index in 0..length {
                vm.tick()?;
                let value = vm.get_property(input.clone(), &index.to_string())?;
                let value = if mapping {
                    vm.invoke(
                        mapper.clone(),
                        args.get(2).cloned().unwrap_or_default(),
                        &[value, Value::from(index)],
                    )?
                } else {
                    value
                };
                vm.define_data_property(
                    result.clone(),
                    &index.to_string(),
                    value,
                    true,
                    true,
                    true,
                )?;
            }
            vm.set_property(result.clone(), "length", Value::from(length))?;
            Ok(result)
        }
    });
}

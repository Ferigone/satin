use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Shortest,
    Longest,
    Strict,
}

#[derive(Clone)]
pub(super) struct Zip {
    mode: Mode,
    keyed: bool,
    records: Vec<IteratorRecord>,
    keys: Vec<PropertyKey>,
    padding: Vec<Value>,
}

impl Zip {
    pub(super) fn extra_bytes(&self) -> usize {
        size_of::<Self>()
            + self.records.capacity() * size_of::<IteratorRecord>()
            + self.keys.capacity() * size_of::<PropertyKey>()
            + self.padding.capacity() * size_of::<Value>()
    }
    pub(super) fn visit(&self, f: &mut impl FnMut(&Value)) {
        for r in &self.records {
            f(&r.iterator);
            f(&r.next);
        }
        for v in &self.padding {
            f(v);
        }
        for key in &self.keys {
            match key {
                PropertyKey::Symbol(s) => f(&Value::Symbol(s.clone())),
                PropertyKey::String(s) => f(&Value::String(s.clone())),
            }
        }
    }

    pub(super) fn close(&mut self, vm: &mut Vm, mut result: ResultValue) -> ResultValue {
        for record in self.records.iter_mut().rev() {
            result = vm.iterator_close(record, result);
        }
        result
    }

    pub(super) fn step(&mut self, vm: &mut Vm) -> Result<Option<Value>, JsError> {
        let mut values = Vec::new();
        let mut exhausted = 0;
        let n = self.records.len();
        vm.reserve_allocation(n * size_of::<Value>())?;
        for i in 0..n {
            let step = match vm.iterator_step(&mut self.records[i]) {
                Ok(v) => v,
                Err(e) => return Err(self.close(vm, Err(e)).unwrap_err()),
            };
            match step {
                Some(v) => {
                    if self.mode == Mode::Strict && exhausted != 0 {
                        return Err(self
                            .close(vm, Err(invalid("Zip inputs have different lengths")))
                            .unwrap_err());
                    }
                    values.push(v);
                }
                None => {
                    exhausted += 1;
                    if self.mode == Mode::Shortest {
                        self.close(vm, Ok(Value::Undefined))?;
                        return Ok(None);
                    }
                    if self.mode == Mode::Strict && exhausted != i + 1 {
                        return Err(self
                            .close(vm, Err(invalid("Zip inputs have different lengths")))
                            .unwrap_err());
                    }
                    values.push(self.padding.get(i).cloned().unwrap_or_default());
                }
            }
        }
        if exhausted == n {
            return Ok(None);
        }
        if !self.keyed {
            return vm.try_new_array(&values).map(Some);
        }
        let result = vm.new_object_with_prototype(Value::Null)?;
        for (key, v) in self.keys.iter().zip(values) {
            vm.define_key(result.clone(), key.clone(), Property::data(v))?;
        }
        Ok(Some(result))
    }

    fn add(&mut self, vm: &mut Vm, value: Value) -> Result<(), JsError> {
        vm.reserve_allocation(2 * (size_of::<IteratorRecord>() + size_of::<PropertyKey>()))?;
        self.records.push(flattenable(vm, value, false)?);
        Ok(())
    }

    fn read_inputs(&mut self, vm: &mut Vm, inputs: Value) -> Result<(), JsError> {
        if self.keyed {
            let id = inputs.as_object().unwrap();
            for key in vm.own_property_keys(inputs.clone())? {
                if !vm
                    .own_descriptor_key(id, &key)?
                    .is_some_and(|p| p.enumerable)
                {
                    continue;
                }
                let v = vm.get_property_key(inputs.clone(), key.clone())?;
                if !v.is_undefined() {
                    self.add(vm, v)?;
                    self.keys.push(key);
                }
            }
        } else {
            let mut outer = vm.get_iterator(inputs)?;
            while let Some(v) = vm.iterator_step(&mut outer)? {
                if let Err(error) = self.add(vm, v) {
                    let result = self.close(vm, Err(error));
                    return Err(vm.iterator_close(&mut outer, result).unwrap_err());
                }
            }
        }
        Ok(())
    }
    fn read_padding(&mut self, vm: &mut Vm, padding: Value) -> Result<(), JsError> {
        if padding.is_undefined() {
            return Ok(());
        }
        vm.reserve_allocation(self.records.len() * size_of::<Value>())?;
        if self.keyed {
            for key in &self.keys {
                self.padding
                    .push(vm.get_property_key(padding.clone(), key.clone())?);
            }
        } else {
            let mut r = vm.get_iterator(padding)?;
            for _ in &self.records {
                let Some(v) = vm.iterator_step(&mut r)? else {
                    break;
                };
                self.padding.push(v);
            }
            vm.iterator_close(&mut r, Ok(Value::Undefined))?;
        }
        Ok(())
    }
}

pub(super) fn install(vm: &mut Vm, ctor: &Value, hid: ObjectId) {
    for (name, keyed) in [("zip", false), ("zipKeyed", true)] {
        method(vm, ctor.clone(), name, 1, move |vm, _, args| {
            let inputs = arg(args, 0);
            if inputs.as_object().is_none() {
                return Err(invalid("Zip input must be an object"));
            }
            let opts = arg(args, 1);
            if !opts.is_undefined() && opts.as_object().is_none() {
                return Err(invalid("Zip options must be an object"));
            }
            let mode = if opts.is_undefined() {
                Value::Undefined
            } else {
                vm.get_property(opts.clone(), "mode")?
            };
            let mode = match mode {
                Value::Undefined => Mode::Shortest,
                Value::String(s) if s == JsString::from("shortest") => Mode::Shortest,
                Value::String(s) if s == JsString::from("strict") => Mode::Strict,
                Value::String(s) if s == JsString::from("longest") => Mode::Longest,
                _ => return Err(invalid("Invalid zip mode")),
            };
            let padding = if mode == Mode::Longest {
                let p = vm.get_property(opts, "padding")?;
                if !p.is_undefined() && p.as_object().is_none() {
                    return Err(invalid("Zip padding must be an object"));
                }
                p
            } else {
                Value::Undefined
            };
            let mut z = Zip {
                mode,
                keyed,
                records: vec![],
                keys: vec![],
                padding: vec![],
            };
            if let Err(error) = z
                .read_inputs(vm, inputs)
                .and_then(|_| z.read_padding(vm, padding))
            {
                return z.close(vm, Err(error));
            }
            let mut h = Helper::new(Lazy::Zip);
            h.zip = Some(Box::new(z));
            helper(vm, hid, h)
        });
    }
}

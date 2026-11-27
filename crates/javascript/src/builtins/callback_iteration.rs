use super::{
    arg,
    array_iteration::{self, PresentIndices},
};
use crate::{JsError, JsString, Value, Vm, collections, heap::Property};

enum Cursor {
    Array(PresentIndices),
    Collection(u64),
}

struct ArrayMap {
    result: Value,
    key: Option<JsString>,
}

/// Native iteration can run directly or retain its state in an owned JS frame.
pub(crate) struct CallbackIteration {
    target: Value,
    pub(crate) callback: Value,
    pub(crate) receiver: Value,
    cursor: Cursor,
    map: Option<ArrayMap>,
}

impl CallbackIteration {
    pub(crate) fn array(
        vm: &mut Vm,
        this: Value,
        args: &[Value],
        map: bool,
    ) -> Result<Self, JsError> {
        let (target, length, callback) = array_iteration::prepare(vm, this, args)?;
        let map = if map {
            Some(ArrayMap {
                result: vm.array_species_create(target.clone(), length)?,
                key: None,
            })
        } else {
            None
        };
        Ok(Self {
            target,
            callback,
            receiver: arg(args, 1),
            cursor: Cursor::Array(PresentIndices::new(0..length, false)),
            map,
        })
    }

    pub(crate) fn collection(
        vm: &Vm,
        target: Value,
        args: &[Value],
        kind: collections::Kind,
    ) -> Result<Self, JsError> {
        collections::receiver(vm, &target, kind)?;
        let callback = arg(args, 0);
        if !vm.is_callable(&callback) {
            return Err(JsError::type_error("Collection forEach callback is not callable"));
        }
        Ok(Self {
            target,
            callback,
            receiver: arg(args, 1),
            cursor: Cursor::Collection(0),
            map: None,
        })
    }

    pub(crate) fn next(&mut self, vm: &mut Vm) -> Result<Option<[Value; 3]>, JsError> {
        let (value, key) = match &mut self.cursor {
            Cursor::Array(indices) => {
                let Some((index, key)) = indices.next(vm, &self.target)? else {
                    return Ok(None);
                };
                let value = vm.get(self.target.clone(), &key)?;
                if let Some(map) = &mut self.map {
                    map.key = Some(key);
                }
                (value, Value::from(index as f64))
            }
            Cursor::Collection(cursor) => {
                let id = self.target.as_object().expect("branded collection");
                let Some((order, key, value)) = vm.collection_entry(id, *cursor)? else {
                    return Ok(None);
                };
                *cursor = order + 1;
                (value, key)
            }
        };
        Ok(Some([value, key, self.target.clone()]))
    }

    pub(crate) fn run(mut self, vm: &mut Vm) -> Result<Value, JsError> {
        while let Some(args) = self.next(vm)? {
            let value = vm.invoke(self.callback.clone(), self.receiver.clone(), &args)?;
            self.accept(vm, value)?;
        }
        Ok(self.finish())
    }

    pub(crate) fn accept(&mut self, vm: &mut Vm, value: Value) -> Result<(), JsError> {
        if let Some(map) = &mut self.map {
            let key = map.key.take().expect("pending Array.map index");
            vm.define_property(map.result.clone(), key, Property::data(value))?;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Value {
        self.map.map_or(Value::Undefined, |map| map.result)
    }

    pub(crate) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        visit(&self.target);
        visit(&self.callback);
        visit(&self.receiver);
        if let Some(map) = &self.map {
            visit(&map.result);
            if let Some(key) = &map.key {
                visit(&Value::String(key.clone()));
            }
        }
    }
}

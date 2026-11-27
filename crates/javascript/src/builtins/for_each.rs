use super::{
    arg,
    array_iteration::{self, PresentIndices},
};
use crate::{JsError, Value, Vm, collections};

enum Cursor {
    Array(PresentIndices),
    Collection(u64),
}

/// The native loop can run directly or retain this cursor in an owned JS frame.
pub(crate) struct ForEachCall {
    target: Value,
    pub(crate) callback: Value,
    pub(crate) receiver: Value,
    cursor: Cursor,
}

impl ForEachCall {
    pub(crate) fn array(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Self, JsError> {
        let (target, length, callback) = array_iteration::prepare(vm, this, args)?;
        Ok(Self {
            target,
            callback,
            receiver: arg(args, 1),
            cursor: Cursor::Array(PresentIndices::new(0..length, false)),
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
        })
    }

    pub(crate) fn next(&mut self, vm: &mut Vm) -> Result<Option<[Value; 3]>, JsError> {
        let (value, key) = match &mut self.cursor {
            Cursor::Array(indices) => {
                let Some((index, key)) = indices.next(vm, &self.target)? else {
                    return Ok(None);
                };
                (vm.get(self.target.clone(), &key)?, Value::from(index as f64))
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
            vm.invoke(self.callback.clone(), self.receiver.clone(), &args)?;
        }
        Ok(Value::Undefined)
    }

    pub(crate) fn visit(&self, visit: &mut impl FnMut(&Value)) {
        visit(&self.target);
        visit(&self.callback);
        visit(&self.receiver);
    }
}

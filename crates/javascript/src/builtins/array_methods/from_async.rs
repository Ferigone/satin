use super::*;
use crate::{bytecode, vm::Completion};
use std::{cell::LazyCell, rc::Rc};

thread_local! {
    // All realms share immutable code, but instantiate functions in their own
    // global environment. Compile only on the first Array.fromAsync invocation.
    static BODY: LazyCell<Rc<bytecode::Chunk>> = LazyCell::new(|| {
        bytecode::compile(include_str!("from_async.js"), 4 * 1024 * 1024)
            .expect("valid built-in Array.fromAsync implementation")
    });
}

pub(super) fn install(vm: &mut Vm) {
    let ops = vm.new_object_with_prototype(Value::Null).unwrap();
    vm.root(ops.clone());
    method(vm, ops.clone(), "call", 2, |vm, _, a| {
        vm.invoke(arg(a, 0), arg(a, 1), a.get(2..).unwrap_or(&[]))
    });
    method(vm, ops.clone(), "requireObject", 1, |_, _, a| {
        if arg(a, 0).as_object().is_none() {
            return Err(JsError::type_error(
                "Async iterator result must be an object",
            ));
        }
        Ok(Value::Undefined)
    });
    method(vm, ops.clone(), "define", 3, |vm, _, a| {
        let key = vm.to_string(&arg(a, 1))?;
        vm.define_property(arg(a, 0), key, crate::heap::Property::data(arg(a, 2)))?;
        Ok(Value::Undefined)
    });
    method(vm, ops.clone(), "prepare", 3, |vm, _, a| {
        let ctor = arg(a, 0);
        let items = arg(a, 1);
        let mapper = arg(a, 2);
        if !mapper.is_undefined() && !vm.is_callable(&mapper) {
            return Err(JsError::type_error(
                "Array.fromAsync mapper is not callable",
            ));
        }
        let method = vm.get_property_key(items.clone(), vm.well_known_key("asyncIterator"))?;
        let record = if !method.is_nullish() {
            Some(vm.iterator_from_method(items.clone(), method)?)
        } else {
            let method = vm.get_property_key(items.clone(), vm.well_known_key("iterator"))?;
            if method.is_nullish() {
                None
            } else {
                let sync = vm.iterator_from_method(items.clone(), method)?;
                Some(vm.async_from_sync_record(sync)?)
            }
        };
        let state = vm.new_object_with_prototype(Value::Null)?;
        let result = if let Some(record) = record {
            vm.define_data_property(
                state.clone(),
                "iterator",
                record.iterator,
                false,
                false,
                false,
            )?;
            vm.define_data_property(state.clone(), "next", record.next, false, false, false)?;
            if vm.is_constructor(&ctor) {
                vm.construct(ctor, &[])?
            } else {
                vm.try_new_array(&[])?
            }
        } else {
            let (source, len) = array_like(vm, items)?;
            vm.define_data_property(state.clone(), "iterator", Value::Null, false, false, false)?;
            vm.define_data_property(state.clone(), "source", source, false, false, false)?;
            vm.define_data_property(
                state.clone(),
                "length",
                Value::from(len),
                false,
                false,
                false,
            )?;
            if vm.is_constructor(&ctor) {
                vm.construct(ctor, &[Value::from(len)])?
            } else {
                new_array(vm, len)?
            }
        };
        vm.define_data_property(state.clone(), "target", result, false, false, false)?;
        Ok(state)
    });
    let array = vm.get_global("Array").unwrap();
    method(vm, array, "fromAsync", 1, move |vm, this, args| {
        let body = if let Some(body) = vm.host_slot(&ops, 0) {
            body
        } else {
            let chunk = BODY.with(|b| Rc::clone(b));
            vm.retain_compiled_chunk(&chunk)?;
            let Completion::Normal(Some(body)) = vm.run_chunk(&chunk, vm.realm.global_env)? else {
                return Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unable to initialize Array.fromAsync",
                ));
            };
            vm.set_host_slot(ops.clone(), 0, body.clone())?;
            body
        };
        vm.invoke(
            body,
            Value::Undefined,
            &[ops.clone(), this, arg(args, 0), arg(args, 1), arg(args, 2)],
        )
    });
}

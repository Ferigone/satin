//! Explicit resource management. Each stack owns a capability whose head is
//! traced by the heap. Reading a guest disposal method may transfer its stack,
//! so registration captures the capability before invoking any user getter.
use super::{arg, constructor, create_error, method, replace_construct};
use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm, bytecode,
    heap::{ObjectKind, Property, PropertyKind},
    vm::Completion,
};
use std::{cell::LazyCell, rc::Rc};

thread_local! {
    static BODIES:LazyCell<Rc<bytecode::Chunk>>=LazyCell::new(||bytecode::compile(include_str!("disposable/dispose.js"),4*1024*1024).expect("valid disposal algorithms"));
}

fn suppressed(vm: &mut Vm, args: &[Value], proto: ObjectId) -> Result<Value, JsError> {
    let error = create_error(vm, ErrorKind::Error, &[arg(args, 2)], proto)?;
    for (index, key) in ["error", "suppressed"].into_iter().enumerate() {
        vm.define_data_property(error.clone(), key, arg(args, index), true, false, true)?;
    }
    Ok(error)
}
fn install_error(vm: &mut Vm) -> ObjectId {
    let proto = vm
        .new_object_with_prototype(Value::Object(vm.realm.error_prototype))
        .unwrap();
    vm.root(proto.clone());
    vm.define_data_property(
        proto.clone(),
        "name",
        "SuppressedError".into(),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(proto.clone(), "message", "".into(), true, false, true)
        .unwrap();
    let intrinsic = proto.as_object().unwrap();
    let ctor = constructor(vm, "SuppressedError", 3, proto, move |vm, _, args| {
        suppressed(vm, args, intrinsic)
    });
    let error = vm.get_global("Error").unwrap();
    vm.set_prototype(ctor.as_object().unwrap(), error.as_object())
        .unwrap();
    replace_construct(vm, ctor, move |vm, _, args, target| {
        let proto = vm.prototype_from_constructor(target, intrinsic)?;
        suppressed(vm, args, proto)
    });
    intrinsic
}

fn brand(vm: &Vm, stack: &Value, asynchronous: bool) -> Result<bool, JsError> {
    if let Some(id) = stack.as_object()
        && let ObjectKind::DisposableStack {
            asynchronous: kind,
            disposed,
        } = vm.heap.object(id)?.kind
        && kind == asynchronous
    {
        return Ok(disposed);
    }
    Err(JsError::type_error("Invalid disposable stack receiver"))
}
fn capability(vm: &Vm, stack: &Value, asynchronous: bool) -> Result<Value, JsError> {
    if brand(vm, stack, asynchronous)? {
        return Err(JsError::new(
            ErrorKind::ReferenceError,
            "Stack has already been disposed",
        ));
    }
    Ok(vm.host_slot(stack, 0).expect("live stack capability"))
}
fn set_disposed(vm: &mut Vm, stack: &Value) -> Result<(), JsError> {
    let ObjectKind::DisposableStack { disposed, .. } =
        &mut vm.heap.object_mut(stack.as_object().unwrap())?.kind
    else {
        unreachable!()
    };
    *disposed = true;
    vm.set_host_slot(stack.clone(), 0, Value::Undefined)
}
fn stack(vm: &mut Vm, asynchronous: bool, proto: ObjectId, cap: Value) -> Result<Value, JsError> {
    let stack = vm.allocate_constructed(
        ObjectKind::DisposableStack {
            asynchronous,
            disposed: false,
        },
        proto,
    )?;
    vm.set_host_slot(stack.clone(), 0, cap)?;
    Ok(stack)
}
#[derive(Clone, Copy)]
enum Register {
    Use,
    Defer,
    Adopt,
}
fn register(
    vm: &mut Vm,
    this: Value,
    args: &[Value],
    mode: Register,
    asynchronous: bool,
) -> Result<Value, JsError> {
    let cap = capability(vm, &this, asynchronous)?;
    let value = arg(args, 0);
    // Kinds distinguish ordinary calls, use methods, adopt callbacks,
    // synchronous fallback for async disposal and an empty async resource.
    let (callback, kind) = match mode {
        Register::Adopt => (arg(args, 1), 2),
        Register::Defer => (value.clone(), 0),
        Register::Use if value.is_nullish() => {
            if !asynchronous {
                return Ok(value);
            }
            (Value::Undefined, 4)
        }
        Register::Use => {
            if value.as_object().is_none() {
                return Err(JsError::type_error("Disposable value must be an object"));
            }
            let mut callback = Value::Undefined;
            let mut kind = 1;
            if asynchronous {
                callback = vm.get_property_key(value.clone(), vm.well_known_key("asyncDispose"))?;
            }
            if callback.is_nullish() {
                callback = vm.get_property_key(value.clone(), vm.well_known_key("dispose"))?;
                if asynchronous {
                    kind = 3;
                }
            }
            (callback, kind)
        }
    };
    if kind != 4 && !vm.is_callable(&callback) {
        return Err(JsError::type_error("Disposer must be callable"));
    }
    let entry = vm.new_object_with_prototype(Value::Null)?;
    let previous = vm.host_slot(&cap, 0).unwrap_or(Value::Null);
    for (key, v) in [
        ("previous", previous),
        ("kind", Value::from(kind)),
        ("callback", callback),
        ("value", value.clone()),
    ] {
        vm.define_data_property(entry.clone(), key, v, false, false, false)?;
    }
    vm.set_host_slot(cap, 0, entry)?;
    Ok(if matches!(mode, Register::Defer) {
        Value::Undefined
    } else {
        value
    })
}
fn call_body(vm: &mut Vm, ops: &Value, method: &str, receiver: Value) -> Result<Value, JsError> {
    let bodies = if let Some(bodies) = vm.host_slot(ops, 0) {
        bodies
    } else {
        let chunk = BODIES.with(|c| Rc::clone(c));
        vm.retain_compiled_chunk(&chunk)?;
        let Completion::Normal(Some(bodies)) = vm.run_chunk(&chunk, vm.realm.global_env)? else {
            return Err(JsError::new(
                ErrorKind::InternalError,
                "Unable to initialize disposable stacks",
            ));
        };
        vm.set_host_slot(ops.clone(), 0, bodies.clone())?;
        bodies
    };
    let body = vm.get_property(bodies, method)?;
    vm.invoke(body, Value::Undefined, &[ops.clone(), receiver])
}
pub(super) fn install(vm: &mut Vm) {
    let error_proto = install_error(vm);
    let ops = vm.new_object_with_prototype(Value::Null).unwrap();
    vm.root(ops.clone());
    method(vm, ops.clone(), "begin", 2, |vm, _, args| {
        let stack = arg(args, 0);
        if brand(vm, &stack, arg(args, 1).is_truthy())? {
            return Ok(Value::Null);
        }
        let cap = vm.host_slot(&stack, 0).unwrap();
        set_disposed(vm, &stack)?;
        let head = vm.host_slot(&cap, 0).unwrap_or(Value::Null);
        vm.set_host_slot(cap, 0, Value::Null)?;
        Ok(head)
    });
    method(vm, ops.clone(), "invoke", 1, |vm, _, args| {
        let entry = arg(args, 0);
        let kind = vm.get_property(entry.clone(), "kind")?;
        let value = vm.get_property(entry.clone(), "value")?;
        let callback = vm.get_property(entry, "callback")?;
        match kind {
            Value::Number(2.) => vm.invoke(callback, Value::Undefined, &[value]),
            Value::Number(0.) => vm.invoke(callback, Value::Undefined, &[]),
            _ => vm.invoke(callback, value, &[]),
        }
    });
    method(vm, ops.clone(), "suppress", 2, move |vm, _, args| {
        suppressed(vm, args, error_proto)
    });
    method(vm, ops.clone(), "finish", 2, |vm, _, args| {
        vm.invoke(arg(args, 0), arg(args, 1), &[])
    });
    for (name, asynchronous) in [("DisposableStack", false), ("AsyncDisposableStack", true)] {
        let proto = vm.new_object();
        vm.root(proto.clone());
        let intrinsic = proto.as_object().unwrap();
        let ctor = constructor(vm, name, 0, proto.clone(), |_, _, _| {
            Err(JsError::type_error("Disposable stack requires new"))
        });
        replace_construct(vm, ctor, move |vm, _, _, target| {
            let prototype = vm.prototype_from_constructor(target, intrinsic)?;
            let cap = vm.new_object_with_prototype(Value::Null)?;
            stack(vm, asynchronous, prototype, cap)
        });
        for (key, len, kind) in [
            ("adopt", 2, Register::Adopt),
            ("defer", 1, Register::Defer),
            ("use", 1, Register::Use),
        ] {
            method(vm, proto.clone(), key, len, move |vm, this, args| {
                register(vm, this, args, kind, asynchronous)
            });
        }
        method(vm, proto.clone(), "move", 0, move |vm, this, _| {
            let cap = capability(vm, &this, asynchronous)?;
            let moved = stack(vm, asynchronous, intrinsic, cap)?;
            set_disposed(vm, &this)?;
            Ok(moved)
        });
        let get = vm.native(
            "get disposed",
            0,
            false,
            Rc::new(move |vm, this, _| Ok(brand(vm, &this, asynchronous)?.into())),
        );
        vm.define_key(
            proto.clone(),
            "disposed".into(),
            Property {
                kind: PropertyKind::Accessor {
                    get: Some(get),
                    set: None,
                },
                enumerable: false,
                configurable: true,
            },
        )
        .unwrap();
        let (name_method, symbol, body) = if asynchronous {
            ("disposeAsync", "asyncDispose", "async")
        } else {
            ("dispose", "dispose", "sync")
        };
        let private_ops = ops.clone();
        let function = method(vm, proto.clone(), name_method, 0, move |vm, this, _| {
            call_body(vm, &private_ops, body, this)
        });
        vm.define_data_property_key(
            proto.clone(),
            vm.well_known_key(symbol),
            function,
            true,
            false,
            true,
        )
        .unwrap();
        vm.define_data_property_key(
            proto,
            vm.well_known_key("toStringTag"),
            name.into(),
            false,
            false,
            true,
        )
        .unwrap();
    }
    let async_iterator = vm.async_iterator_prototype();
    let function = vm.native(
        "[Symbol.asyncDispose]",
        0,
        false,
        Rc::new(move |vm, it, _| call_body(vm, &ops, "iterator", it)),
    );
    vm.define_data_property_key(
        async_iterator,
        vm.well_known_key("asyncDispose"),
        function,
        true,
        false,
        true,
    )
    .unwrap();
}

//! JavaScript embedding for a bounded, straight-line i32 WebAssembly core.
mod decode;

use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    builtins::{create_error, method, replace_construct},
    heap::{ObjectKind, Property, PropertyKind},
    vm::to_i32,
};
pub(crate) use decode::Module;
use decode::{Op, Signature, allocation_error, compile_error};
use std::{mem::size_of, rc::Rc};

const INSTANCE_EXPORTS: u32 = 0;
const INSTANCE_MODULE: u32 = 1;
const INSTANCE_IMPORTS: u32 = 2;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn scratch<T>(vm: &mut Vm, capacity: usize) -> Result<Vec<T>, JsError> {
    vm.reserve_allocation(capacity.saturating_mul(size_of::<T>()))?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| allocation_error())?;
    Ok(values)
}

fn compile(vm: &mut Vm, bytes: &Value) -> Result<Module, JsError> {
    let size = vm
        .buffer_source_len(bytes)?
        .ok_or_else(|| JsError::type_error("Expected a WebAssembly BufferSource"))?;
    if size > decode::MAX_SOURCE_BYTES {
        return Err(compile_error(
            "WebAssembly source exceeds the supported byte limit",
        ));
    }
    let bytes = vm
        .copy_buffer_source(bytes)?
        .ok_or_else(|| JsError::type_error("Expected a WebAssembly BufferSource"))?;
    decode::decode(vm, bytes)
}

fn create_module(vm: &mut Vm, bytes: &Value, prototype: ObjectId) -> Result<Value, JsError> {
    let module = compile(vm, bytes)?;
    vm.ensure_allocation(module.retained_bytes().saturating_add(384))?;
    vm.allocate_constructed(ObjectKind::WasmModule(Rc::new(module)), prototype)
}

fn module_data(vm: &Vm, module: &Value) -> Option<Rc<Module>> {
    match &vm.heap.object(module.as_object()?).ok()?.kind {
        ObjectKind::WasmModule(module) => Some(module.clone()),
        _ => None,
    }
}

fn validate_import_object(imports: &Value) -> Result<(), JsError> {
    if imports.is_undefined() || imports.as_object().is_some() {
        Ok(())
    } else {
        Err(JsError::type_error(
            "WebAssembly import object must be an object",
        ))
    }
}

fn link(vm: &mut Vm, module: &Module, import_object: &Value) -> Result<Value, JsError> {
    validate_import_object(import_object)?;
    if !module.imports.is_empty() && import_object.is_undefined() {
        return Err(JsError::type_error(
            "WebAssembly module requires an import object",
        ));
    }
    let mut functions = scratch(vm, module.imports.len())?;
    for import in &module.imports {
        let namespace = vm.get_property(import_object.clone(), module.name(&import.module))?;
        if namespace.as_object().is_none() {
            return Err(JsError::type_error(
                "WebAssembly import namespace must be an object",
            ));
        }
        let function = vm.get_property(namespace, module.name(&import.field))?;
        if !vm.is_callable(&function) {
            return Err(JsError::new(
                ErrorKind::LinkError,
                "WebAssembly function import is not callable",
            ));
        }
        functions.push(function);
    }
    vm.try_new_array(&functions)
}

fn call_import(
    vm: &mut Vm,
    imports: &Value,
    index: usize,
    signature: Signature,
    parameters: &[i32],
) -> Result<Option<i32>, JsError> {
    let callback = vm.get_property(imports.clone(), &index.to_string())?;
    let mut arguments = scratch(vm, parameters.len())?;
    arguments.extend(parameters.iter().map(|number| Value::from(*number)));
    let value = vm.invoke(callback, Value::Undefined, &arguments)?;
    if signature.result {
        Ok(Some(to_i32(vm.to_number(&value)?)))
    } else {
        Ok(None)
    }
}

fn invoke_export(
    vm: &mut Vm,
    module_object: &Value,
    imports: &Value,
    index: usize,
    arguments: &[Value],
) -> Result<Value, JsError> {
    let module = module_data(vm, module_object).expect("traced WebAssembly module");
    let signature = module.signature(index);
    let function = index
        .checked_sub(module.imports.len())
        .map(|index| &module.functions[index]);
    let mut locals = scratch(
        vm,
        function.map_or(signature.params, |function| function.locals),
    )?;
    for parameter in 0..signature.params {
        let argument = arguments.get(parameter).unwrap_or(&Value::Undefined);
        locals.push(to_i32(vm.to_number(argument)?));
    }
    let Some(function) = function else {
        return Ok(call_import(vm, imports, index, signature, &locals)?
            .map_or(Value::Undefined, Value::from));
    };
    locals.resize(function.locals, 0);
    let mut stack = scratch(vm, function.stack)?;
    for op in &function.code {
        vm.consume_work(1)?;
        match *op {
            Op::LocalGet(index) => stack.push(locals[index]),
            Op::LocalSet(index) => locals[index] = stack.pop().expect("validated operand stack"),
            Op::LocalTee(index) => locals[index] = *stack.last().expect("validated operand stack"),
            Op::Const(value) => stack.push(value),
            Op::CallImport(index) => {
                let signature = module.imports[index].signature;
                let start = stack.len() - signature.params;
                let result = call_import(vm, imports, index, signature, &stack[start..])?;
                stack.truncate(start);
                stack.extend(result);
            }
            Op::Drop => {
                stack.pop();
            }
            Op::Add | Op::Sub | Op::Mul => {
                let right = stack.pop().expect("validated operand stack");
                let left = stack.last_mut().expect("validated operand stack");
                *left = match op {
                    Op::Add => left.wrapping_add(right),
                    Op::Sub => left.wrapping_sub(right),
                    Op::Mul => left.wrapping_mul(right),
                    _ => unreachable!("validated i32 arithmetic instruction"),
                };
            }
        }
    }
    Ok(stack.pop().map_or(Value::Undefined, Value::from))
}

fn create_instance(
    vm: &mut Vm,
    module_object: Value,
    import_object: &Value,
    prototype: ObjectId,
) -> Result<Value, JsError> {
    let module = module_data(vm, &module_object)
        .ok_or_else(|| JsError::type_error("Expected a WebAssembly.Module"))?;
    let imports = link(vm, &module, import_object)?;
    vm.ensure_allocation(384)?;
    let exports = vm.new_object_with_prototype(Value::Null)?;
    let count = module.imports.len() + module.functions.len();
    let mut wrappers: Vec<Option<Value>> = scratch(vm, count)?;
    wrappers.resize(count, None);
    for export in &module.exports {
        let index = export.function;
        if wrappers[index].is_none() {
            vm.ensure_allocation(1024)?;
            let captured_module = module_object.clone();
            let captured_imports = imports.clone();
            let wrapper = vm.native(
                &index.to_string(),
                module.signature(index).params,
                false,
                Rc::new(move |vm, _, args| {
                    invoke_export(vm, &captured_module, &captured_imports, index, args)
                }),
            );
            // A kept export keeps its module and JavaScript imports alive.
            vm.set_host_slot(wrapper.clone(), INSTANCE_MODULE, module_object.clone())?;
            vm.set_host_slot(wrapper.clone(), INSTANCE_IMPORTS, imports.clone())?;
            wrappers[index] = Some(wrapper);
        }
        vm.define_data_property(
            exports.clone(),
            module.name(&export.name),
            wrappers[index].as_ref().expect("installed export").clone(),
            false,
            true,
            false,
        )?;
    }
    vm.prevent_extensions(exports.as_object().unwrap())?;
    let result = vm.allocate_constructed(ObjectKind::WasmInstance, prototype)?;
    vm.set_host_slot(result.clone(), INSTANCE_EXPORTS, exports)?;
    vm.set_host_slot(result.clone(), INSTANCE_MODULE, module_object)?;
    vm.set_host_slot(result.clone(), INSTANCE_IMPORTS, imports)?;
    Ok(result)
}

fn settle(vm: &mut Vm, promise: ObjectId, result: Result<Value, JsError>) -> Result<(), JsError> {
    match result {
        Ok(value) => vm.resolve_promise(promise, value),
        Err(error) => {
            let reason = vm.error_reason(error)?;
            vm.settle_promise(promise, reason, true)
        }
    }
}

fn instantiate(
    vm: &mut Vm,
    source: Value,
    imports: Value,
    module_proto: ObjectId,
    instance_proto: ObjectId,
) -> Result<Value, JsError> {
    let promise = vm.new_promise_capability()?.promise;
    let promise_id = promise.as_object().unwrap();
    if module_data(vm, &source).is_some() {
        let result = create_instance(vm, source, &imports, instance_proto);
        settle(vm, promise_id, result)?;
        return Ok(promise);
    }
    let module = match validate_import_object(&imports)
        .and_then(|()| create_module(vm, &source, module_proto))
    {
        Ok(module) => module,
        Err(error) => {
            settle(vm, promise_id, Err(error))?;
            return Ok(promise);
        }
    };
    vm.ensure_allocation(1024)?;
    let captured_module = module.clone();
    let captured_imports = imports.clone();
    let instantiate =
        vm.new_native_function("Complete WebAssembly instantiation", move |vm, _, _| {
            let result = (|| {
                let instance = create_instance(
                    vm,
                    captured_module.clone(),
                    &captured_imports,
                    instance_proto,
                )?;
                vm.ensure_allocation(1024)?;
                let result = vm.new_object();
                vm.define_data_property(
                    result.clone(),
                    "module",
                    captured_module.clone(),
                    true,
                    true,
                    true,
                )?;
                vm.define_data_property(result.clone(), "instance", instance, true, true, true)?;
                Ok(result)
            })();
            settle(vm, promise_id, result)?;
            Ok(Value::Undefined)
        });
    vm.set_host_slot(instantiate.clone(), 0, module)?;
    vm.set_host_slot(instantiate.clone(), 1, imports)?;
    vm.set_host_slot(instantiate.clone(), 2, promise.clone())?;
    vm.set_host_slot(instantiate.clone(), 3, Value::Object(instance_proto))?;
    vm.enqueue_microtask(instantiate)?;
    Ok(promise)
}

fn class(
    vm: &mut Vm,
    namespace: &Value,
    name: &'static str,
    prototype: &Value,
    call_without_new: bool,
    create: impl Fn(&mut Vm, ObjectId, &[Value]) -> Result<Value, JsError> + 'static,
) -> Value {
    let prototype_id = prototype.as_object().unwrap();
    let create = Rc::new(create);
    let call = create.clone();
    let constructor = vm.native(
        name,
        1,
        false,
        Rc::new(move |vm, _, args| {
            if !call_without_new {
                return Err(JsError::type_error("WebAssembly constructor requires new"));
            }
            call(vm, prototype_id, args)
        }),
    );
    replace_construct(vm, constructor.clone(), move |vm, _, args, target| {
        let prototype = vm.prototype_from_constructor(target, prototype_id)?;
        create(vm, prototype, args)
    });
    vm.define_data_property(
        constructor.clone(),
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
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        namespace.clone(),
        name,
        constructor.clone(),
        true,
        false,
        true,
    )
    .unwrap();
    constructor
}

fn tag(vm: &mut Vm, target: &Value, name: &'static str) {
    vm.define_data_property_key(
        target.clone(),
        vm.well_known_key("toStringTag"),
        Value::from(name),
        false,
        false,
        true,
    )
    .unwrap();
}

pub(crate) fn install(vm: &mut Vm) {
    let namespace = vm.new_object();
    tag(vm, &namespace, "WebAssembly");
    let module = vm.new_object();
    let instance = vm.new_object();
    tag(vm, &module, "WebAssembly.Module");
    tag(vm, &instance, "WebAssembly.Instance");
    let module_proto = module.as_object().unwrap();
    let instance_proto = instance.as_object().unwrap();
    class(
        vm,
        &namespace,
        "Module",
        &module,
        false,
        |vm, prototype, args| create_module(vm, &arg(args, 0), prototype),
    );
    class(
        vm,
        &namespace,
        "Instance",
        &instance,
        false,
        |vm, prototype, args| create_instance(vm, arg(args, 0), &arg(args, 1), prototype),
    );
    let get_exports = vm.new_native_function("get exports", |vm, this, _| {
        let branded = this.as_object().is_some_and(|id| {
            vm.heap
                .object(id)
                .is_ok_and(|o| matches!(o.kind, ObjectKind::WasmInstance))
        });
        if !branded {
            return Err(JsError::type_error(
                "WebAssembly.Instance exports requires an instance",
            ));
        }
        Ok(vm
            .host_slot(&this, INSTANCE_EXPORTS)
            .expect("initialized WebAssembly exports"))
    });
    vm.define_property(
        instance,
        "exports".into(),
        Property {
            kind: PropertyKind::Accessor {
                get: Some(get_exports),
                set: None,
            },
            enumerable: true,
            configurable: true,
        },
    )
    .unwrap();
    let error_constructor = vm.get_global("Error").unwrap().as_object();
    for (name, kind) in [
        ("CompileError", ErrorKind::CompileError),
        ("LinkError", ErrorKind::LinkError),
    ] {
        let prototype = vm
            .new_object_with_prototype(Value::Object(vm.error_prototype))
            .unwrap();
        vm.error_prototypes
            .insert(kind, prototype.as_object().unwrap());
        vm.define_data_property(
            prototype.clone(),
            "name",
            Value::from(name),
            true,
            false,
            true,
        )
        .unwrap();
        vm.define_data_property(
            prototype.clone(),
            "message",
            Value::from(""),
            true,
            false,
            true,
        )
        .unwrap();
        let ctor = class(
            vm,
            &namespace,
            name,
            &prototype,
            true,
            move |vm, prototype, args| create_error(vm, kind, args, prototype),
        );
        vm.set_prototype(ctor.as_object().unwrap(), error_constructor)
            .unwrap();
    }
    method(
        vm,
        namespace.clone(),
        "validate",
        1,
        |vm, _, args| match compile(vm, &arg(args, 0)) {
            Ok(_) => Ok(Value::Bool(true)),
            Err(error) if error.kind == ErrorKind::CompileError => Ok(Value::Bool(false)),
            Err(error) => Err(error),
        },
    );
    let compile = method(vm, namespace.clone(), "compile", 1, move |vm, _, args| {
        let promise = vm.new_promise_capability()?.promise;
        let result = create_module(vm, &arg(args, 0), module_proto);
        settle(vm, promise.as_object().unwrap(), result)?;
        Ok(promise)
    });
    vm.set_host_slot(compile, 0, Value::Object(module_proto))
        .unwrap();
    let instantiate = method(
        vm,
        namespace.clone(),
        "instantiate",
        1,
        move |vm, _, args| {
            instantiate(vm, arg(args, 0), arg(args, 1), module_proto, instance_proto)
        },
    );
    vm.set_host_slot(instantiate.clone(), 0, Value::Object(module_proto))
        .unwrap();
    vm.set_host_slot(instantiate, 1, Value::Object(instance_proto))
        .unwrap();
    vm.define_data_property(vm.global(), "WebAssembly", namespace, true, false, true)
        .unwrap();
}

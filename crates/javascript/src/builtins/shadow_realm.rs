//! Shadow realms use ordinary realm intrinsics. Values cross the boundary
//! through primitive copies or newly allocated, non-constructable callables.
use super::{arg, constructor, method, replace_construct};
use crate::{
    ErrorKind, JsError, RealmId, Value, Vm, bytecode,
    heap::{EnvKind, Environment},
    vm::Completion,
};
use std::rc::Rc;

struct Shadow(RealmId);

fn boundary_error(error: JsError) -> JsError {
    if error.uncatchable() {
        error
    } else {
        JsError::type_error("ShadowRealm operation threw")
    }
}

fn receiver(vm: &Vm, this: &Value) -> Result<RealmId, JsError> {
    vm.host_data::<Shadow>(this)
        .map(|r| r.0)
        .ok_or_else(|| JsError::type_error("Incompatible ShadowRealm receiver"))
}

fn wrap(vm: &mut Vm, value: Value) -> Result<Value, JsError> {
    let Some(id) = value.as_object() else {
        return Ok(value);
    };
    if !vm.is_callable(&value) {
        return Err(JsError::type_error(
            "Only primitives and callables can cross a ShadowRealm boundary",
        ));
    }
    let target_realm = vm.callable_realm(id).map_err(boundary_error)?;
    let length = if vm
        .own_descriptor_key(id, &"length".into())
        .map_err(boundary_error)?
        .is_some()
    {
        match vm
            .get_property(value.clone(), "length")
            .map_err(boundary_error)?
        {
            Value::Number(n) => n.trunc().max(0.0),
            _ => 0.0,
        }
    } else {
        0.0
    };
    let name = match vm
        .get_property(value.clone(), "name")
        .map_err(boundary_error)?
    {
        s @ Value::String(_) => s,
        _ => Value::from(""),
    };
    vm.reserve_allocation(512)?;
    let wrapper = vm.new_native_function("", move |vm, this, args| {
        let result = vm
            .with_realm(target_realm, |vm| {
                vm.reserve_allocation(std::mem::size_of_val(args))?;
                let mut inner = Vec::with_capacity(args.len());
                for value in args {
                    vm.tick()?;
                    inner.push(wrap(vm, value.clone())?);
                }
                let this = wrap(vm, this)?;
                vm.invoke(Value::Object(id), this, &inner)
            })
            .map_err(boundary_error)?;
        wrap(vm, result)
    });
    vm.set_host_slot(wrapper.clone(), 0, value)?;
    vm.define_data_property(
        wrapper.clone(),
        "length",
        Value::Number(length),
        false,
        false,
        true,
    )?;
    vm.define_data_property(wrapper.clone(), "name", name, false, false, true)?;
    Ok(wrapper)
}

fn evaluate(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let realm = receiver(vm, &this)?;
    let Value::String(source) = arg(args, 0) else {
        return Err(JsError::type_error(
            "ShadowRealm.evaluate requires a string",
        ));
    };
    vm.consume_work(source.len().saturating_mul(2))?;
    let source = String::from_utf16(source.as_utf16()).map_err(|_| {
        JsError::new(
            ErrorKind::UnsupportedFeature,
            "Isolated surrogate code units in script source are unsupported",
        )
    })?;
    // Syntax errors from the source belong to the caller. A nested eval or
    // user exception after execution begins is caught at the realm boundary.
    let chunk = bytecode::compile(&source, vm.compilation_budget(&source)?)?;
    vm.retain_compiled_chunk(&chunk)?;
    let value = vm
        .with_realm(realm, |vm| {
            let kind = if chunk.strict {
                EnvKind::Function
            } else {
                EnvKind::SloppyEval
            };
            let scope =
                vm.heap
                    .allocate_env(Environment::new(Some(vm.realm.global_env), kind, None));
            match vm.run_chunk(&chunk, scope)? {
                Completion::Normal(v) => Ok(v.unwrap_or_default()),
                _ => Err(JsError::new(
                    ErrorKind::InternalError,
                    "Unexpected shadow evaluation completion",
                )),
            }
        })
        .map_err(boundary_error)?;
    wrap(vm, value)
}

fn import_value(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value, JsError> {
    let realm = receiver(vm, &this)?;
    let specifier = vm.to_string(&arg(args, 0))?;
    let Value::String(name) = arg(args, 1) else {
        return Err(JsError::type_error(
            "ShadowRealm import requires a string export name",
        ));
    };
    let inner = vm.with_realm(realm, |vm| {
        vm.dynamic_import(
            crate::ImportPhase::Evaluation,
            Value::String(specifier),
            Value::Undefined,
        )
    })?;
    let success = vm.new_native_function("", move |vm, _, args| {
        let namespace = arg(args, 0);
        let key = crate::PropertyKey::String(name.clone());
        let id = namespace
            .as_object()
            .ok_or_else(|| JsError::type_error("Expected a module namespace"))?;
        let value = vm
            .with_realm(realm, |vm| {
                if vm.own_descriptor_key(id, &key)?.is_none() {
                    return Err(JsError::type_error("Missing ShadowRealm export"));
                }
                vm.get_property_key(namespace, key)
            })
            .map_err(boundary_error)?;
        wrap(vm, value)
    });
    vm.set_host_slot(success.clone(), 0, this)?;
    let rejected = vm.new_native_function("", |_, _, _| {
        Err(JsError::type_error("ShadowRealm module loading failed"))
    });
    vm.then_intrinsic(inner, success, rejected)
}

pub(super) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("toStringTag"),
        Value::from("ShadowRealm"),
        false,
        false,
        true,
    )
    .unwrap();
    let ctor = constructor(vm, "ShadowRealm", 0, prototype.clone(), |_, _, _| {
        Err(JsError::type_error("ShadowRealm requires new"))
    });
    replace_construct(vm, ctor, move |vm, _, _, target| {
        let proto = vm.prototype_from_constructor(target, intrinsic)?;
        let object = vm.new_object_with_prototype(Value::Object(proto))?;
        let loader = vm
            .realm
            .inherit_dynamic_import_hook
            .then(|| vm.realm.dynamic_import_hook.clone())
            .flatten();
        let (realm, global) = vm.new_realm()?;
        vm.with_realm(realm, |vm| {
            vm.realm.inherit_dynamic_import_hook = loader.is_some();
            vm.realm.dynamic_import_hook = loader;
            Ok(())
        })?;
        vm.set_host_data(object.clone(), Rc::new(Shadow(realm)))?;
        vm.set_host_slot(object.clone(), 0, global)?;
        Ok(object)
    });
    method(vm, prototype.clone(), "evaluate", 1, evaluate);
    method(vm, prototype, "importValue", 2, import_value);
}

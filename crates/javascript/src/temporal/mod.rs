//! Temporal's calendar and time calculations run as self-hosted standard-library
//! code in Satin. Each realm owns its constructors, brands and cached zone data.
mod calendar;
mod timezone;
pub(crate) mod slots;

use crate::{
    ErrorKind, JsError, ObjectId, PropertyKey, Value, Vm, bytecode,
    heap::{ObjectKind, PropertyKind},
    vm::Completion,
};
use std::{cell::OnceCell, rc::Rc};

const SOURCE: &str = include_str!("implementation.js");
// The factory receives its realm's constructors, even if a script later
// rebinds one of the corresponding global variables.
const IMPORTS: &[&str] = &[
    "Object",
    "Array",
    "Date",
    "Math",
    "Number",
    "BigInt",
    "String",
    "Symbol",
    "RegExp",
    "RangeError",
    "TypeError",
    "Error",
    "Map",
    "WeakMap",
    "Set",
    "Reflect",
    "Intl",
];
const LOADER: u32 = 20;
const DATE_METHOD: u32 = 21;
const CLASSES: &[&str] = &[
    "PlainYearMonth",
    "PlainMonthDay",
    "PlainDate",
    "PlainTime",
    "PlainDateTime",
    "ZonedDateTime",
    "Instant",
    "Duration",
    "Now",
];

// Only immutable executable code is shared. Every evaluation produces new
// functions, WeakMaps and objects in the requesting realm.
thread_local! {
    static PROGRAM: OnceCell<Result<Rc<bytecode::Chunk>, JsError>> = const { OnceCell::new() };
}

pub(crate) fn install(vm: &mut Vm) {
    let namespace = vm.new_object();
    let id = namespace.as_object().unwrap();
    for (index, name) in ["TypeError", "RangeError", "Error"].into_iter().enumerate() {
        let error = vm.get_global(name).unwrap();
        vm.set_host_slot(namespace.clone(), index as u32, error)
            .unwrap();
    }
    let loader = vm.new_native_function("", move |vm, _, _| initialize(vm, id));
    vm.set_host_slot(namespace.clone(), LOADER, loader).unwrap();
    vm.define_data_property_key(
        namespace.clone(),
        vm.well_known_key("toStringTag"),
        "Temporal".into(),
        false,
        false,
        true,
    )
    .unwrap();
    vm.define_data_property(
        vm.global(),
        "Temporal",
        namespace.clone(),
        true,
        false,
        true,
    )
    .unwrap();

    let date = vm.get_global("Date").unwrap();
    let date_proto = vm.get_property(date, "prototype").unwrap();
    let method = crate::builtins::method(
        vm,
        date_proto,
        "toTemporalInstant",
        0,
        move |vm, this, _| {
            if !this.as_object().is_some_and(|id| {
                matches!(vm.heap.object(id).map(|o| &o.kind), Ok(ObjectKind::Date(_)))
            }) {
                return Err(JsError::type_error(
                    "Date.prototype.toTemporalInstant requires a Date",
                ));
            }
            materialize(vm, id)?;
            let method = vm
                .host_slot(&Value::Object(id), DATE_METHOD)
                .expect("Temporal date method installed");
            vm.call(method, this, &[])
        },
    );
    vm.set_host_slot(method, 0, namespace).unwrap();
    vm.heap.object_mut(id).unwrap().kind = ObjectKind::DeferredTemporal;
}

pub(crate) fn force_property(vm: &mut Vm, key: &PropertyKey, value: &Value) -> Result<(), JsError> {
    if let PropertyKey::String(key) = key {
        force_named(vm, key, value)?;
    }
    Ok(())
}

pub(crate) fn force_named(
    vm: &mut Vm,
    key: &crate::JsString,
    value: &Value,
) -> Result<(), JsError> {
    const NAME: [u16; 8] = [84, 101, 109, 112, 111, 114, 97, 108];
    if key.len() == NAME.len()
        && key.as_utf16() == NAME
        && let Some(id) = value.as_object()
    {
        materialize(vm, id)?;
    }
    Ok(())
}

fn materialize(vm: &mut Vm, id: ObjectId) -> Result<(), JsError> {
    if matches!(vm.heap.object(id)?.kind, ObjectKind::InitializingTemporal) {
        return Err(JsError::type_error(
            "Temporal initialization is already in progress",
        ));
    }
    if matches!(vm.heap.object(id)?.kind, ObjectKind::DeferredTemporal) {
        let loader = vm
            .host_slot(&Value::Object(id), LOADER)
            .expect("Temporal factory installed");
        vm.call(loader, Value::Undefined, &[])?;
    }
    Ok(())
}

fn initialize(vm: &mut Vm, id: ObjectId) -> Result<Value, JsError> {
    if !matches!(vm.heap.object(id)?.kind, ObjectKind::DeferredTemporal) {
        return Err(JsError::type_error(
            "Temporal initialization is already in progress",
        ));
    }
    vm.heap.object_mut(id)?.kind = ObjectKind::InitializingTemporal;
    let result = build(vm, id);
    vm.heap.object_mut(id)?.kind = if result.is_ok() {
        ObjectKind::Ordinary
    } else {
        ObjectKind::DeferredTemporal
    };
    result
}

fn build(vm: &mut Vm, id: ObjectId) -> Result<Value, JsError> {
    // The internal realm keeps its own pristine prototypes. Monkey-patching
    // WeakMap, Number, Array, etc. in a page cannot expose our internal slots
    // or substitute the algorithms used to calculate calendar fields.
    let namespace = Value::Object(id);
    let public_errors = vm.realm.error_prototypes.clone();
    let (realm, _) = vm.new_realm()?;
    let (classes, date_method) = vm.with_realm(realm, |vm| {
        vm.realm.error_prototypes.clone_from(&public_errors);
        let program = PROGRAM.with(|c| {
            c.get_or_init(|| bytecode::compile(SOURCE, 64 * 1024 * 1024))
                .clone()
        })?;
        vm.retain_compiled_chunk(&program)?;
        let Completion::Normal(Some(factory)) = vm.run_chunk(&program, vm.realm.global_env)? else {
            return Err(JsError::new(
                ErrorKind::InternalError,
                "Invalid Temporal factory",
            ));
        };
        let mut inputs = Vec::with_capacity(IMPORTS.len() + 1);
        for &name in IMPORTS {
            let error_slot = ["TypeError", "RangeError", "Error"]
                .iter()
                .position(|e| *e == name);
            let intrinsic = match error_slot {
                Some(slot) => vm.host_slot(&namespace, slot as u32).unwrap(),
                None => vm.get_global(name)?,
            };
            inputs.push(intrinsic);
        }
        let intl = inputs.last().unwrap().clone();
        let formatter = if intl.is_undefined() {
            Value::Undefined
        } else {
            vm.get_property(intl, "DateTimeFormat")?
        };
        inputs.push(formatter);
        inputs.push(vm.new_native_function("", calendar::fields));
        inputs.push(vm.new_native_function("", calendar::year));
        inputs.push(vm.new_native_function("", timezone::open));
        inputs.extend(slots::intrinsics(vm));
        let result = vm.call(factory, Value::Undefined, &inputs)?;
        Ok((
            vm.get_property(result.clone(), "0")?,
            vm.get_property(result, "1")?,
        ))
    })?;
    let mut entries = Vec::with_capacity(CLASSES.len());
    for &name in CLASSES {
        let member = vm.get_property(classes.clone(), name)?;
        let id = member.as_object().expect("Temporal intrinsic is an object");
        if vm.is_callable(&member) {
            let prototype = vm
                .get_property(member.clone(), "prototype")?
                .as_object()
                .unwrap();
            expose(vm, prototype, vm.realm.object_prototype)?;
            expose(vm, id, vm.realm.function_prototype)?;
        } else {
            expose(vm, id, vm.realm.object_prototype)?;
        }
        entries.push((name, member));
    }
    vm.set_host_slot(namespace.clone(), DATE_METHOD, date_method)?;
    for (name, value) in entries {
        vm.define_data_property(namespace.clone(), name, value, true, false, true)?;
    }
    Ok(namespace)
}

// Public methods and prototypes belong to the consumer's object graph. Only
// their closed-over lexical environments access the private implementations.
fn expose(vm: &mut Vm, id: ObjectId, prototype: ObjectId) -> Result<(), JsError> {
    let object = vm.heap.object_mut(id)?;
    object.prototype = Some(prototype);
    let functions: Vec<_> = object
        .properties
        .values()
        .chain(object.symbols.values())
        .flat_map(|p| match &p.kind {
            PropertyKind::Data { value, .. } => [Some(value.clone()), None],
            PropertyKind::Accessor { get, set } => [get.clone(), set.clone()],
        })
        .flatten()
        .collect();
    for function in functions {
        if vm.is_callable(&function) {
            vm.heap.object_mut(function.as_object().unwrap())?.prototype =
                Some(vm.realm.function_prototype);
        }
    }
    Ok(())
}

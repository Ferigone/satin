//! The standard queuing strategies are shared by all stream controllers.
use super::{JsError, Value, Vm, accessor, arg, webidl};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Count,
    ByteLength,
}

struct Strategy {
    kind: Kind,
    high_water: f64,
}

fn branded(vm: &Vm, object: &Value, kind: Kind) -> Result<Rc<Strategy>, JsError> {
    vm.host_data::<Strategy>(object)
        .filter(|strategy| strategy.kind == kind)
        .ok_or_else(|| JsError::type_error("Illegal queuing strategy receiver"))
}

pub(super) fn install(vm: &mut Vm) -> Result<(), JsError> {
    for (name, kind) in [
        ("CountQueuingStrategy", Kind::Count),
        ("ByteLengthQueuingStrategy", Kind::ByteLength),
    ] {
        let prototype = vm.new_object();
        let intrinsic = vm.root(prototype.clone());
        webidl::interface(vm, prototype.clone(), name, 1, move |vm, _, args, new| {
            let target = new.ok_or_else(|| JsError::type_error(format!("{name} requires new")))?;
            webidl::required(args, 1, name)?;
            let options = arg(args, 0);
            if options.as_object().is_none() {
                return Err(JsError::type_error(format!(
                    "{name} requires a highWaterMark"
                )));
            }
            let high_water = vm.get_property(options, "highWaterMark")?;
            if high_water.is_undefined() {
                return Err(JsError::type_error(format!(
                    "{name} requires a highWaterMark"
                )));
            }
            let high_water = vm.to_number(&high_water)?;
            let object = webidl::instance(vm, target, &vm.root_value(intrinsic).unwrap())?;
            vm.set_host_data(object.clone(), Rc::new(Strategy { kind, high_water }))?;
            Ok(object)
        })?;
        accessor(
            vm,
            prototype.clone(),
            "highWaterMark",
            move |vm, this, _| Ok(branded(vm, &this, kind)?.high_water.into()),
        )?;
        let size = vm.new_native_function("size", move |vm, _, args| match kind {
            Kind::Count => Ok(1.into()),
            Kind::ByteLength => vm.get_property(arg(args, 0), "byteLength"),
        });
        if kind == Kind::ByteLength {
            vm.define_data_property(size.clone(), "length", 1.into(), false, false, true)?;
        }
        let size = vm.root(size);
        accessor(vm, prototype, "size", move |vm, this, _| {
            branded(vm, &this, kind)?;
            Ok(vm.root_value(size).unwrap())
        })?;
    }
    Ok(())
}

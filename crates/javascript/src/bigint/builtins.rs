use super::{Integer, JsBigInt, Repr};
use crate::{
    ErrorKind, JsError, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::ObjectKind,
};
use num_traits::{FromPrimitive, ToPrimitive};

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or_default()
}

fn receiver(vm: &Vm, value: Value) -> Result<JsBigInt, JsError> {
    if let Value::BigInt(value) = value {
        return Ok(value);
    }
    if let Some(id) = value.as_object()
        && let ObjectKind::Boxed(Value::BigInt(value)) = &vm.heap.object(id)?.kind
    {
        return Ok(value.clone());
    }
    Err(JsError::type_error(
        "BigInt method requires a BigInt receiver",
    ))
}

pub(crate) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    vm.bigint_prototype = prototype.as_object();
    vm.root(prototype.clone());
    let constructor = constructor(vm, "BigInt", 1, prototype.clone(), |vm, _, args| {
        let primitive = vm.coerce_primitive(arg(args, 0), false)?;
        if let Value::Number(number) = primitive {
            if !number.is_finite() || number.fract() != 0.0 {
                return Err(JsError::new(
                    ErrorKind::RangeError,
                    "BigInt requires an integral Number",
                ));
            }
            vm.prepare_bigint_work(1024, 32)?;
            return vm.bigint_result(Integer::from_f64(number).expect("finite integral Number"));
        }
        Ok(Value::BigInt(vm.bigint_from_primitive(primitive)?))
    });
    replace_construct(vm, constructor.clone(), |_, _, _, _| {
        Err(JsError::type_error("BigInt cannot be called with new"))
    });
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("toStringTag"),
        "BigInt".into(),
        false,
        false,
        true,
    )
    .unwrap();
    for (name, signed) in [("asIntN", true), ("asUintN", false)] {
        method(vm, constructor.clone(), name, 2, move |vm, _, args| {
            let width = crate::binary::to_index(vm, &arg(args, 0))?;
            let value = vm.to_bigint(&arg(args, 1))?;
            vm.wrap_bigint(value, width, signed)
        });
    }
    method(vm, prototype.clone(), "valueOf", 0, |vm, this, _| {
        Ok(Value::BigInt(receiver(vm, this)?))
    });
    method(vm, prototype.clone(), "toString", 0, |vm, this, args| {
        let value = receiver(vm, this)?;
        let radix = arg(args, 0);
        let radix = if radix.is_undefined() {
            10.0
        } else {
            vm.to_number(&radix)?.trunc()
        };
        if !(2.0..=36.0).contains(&radix) {
            return Err(JsError::new(
                ErrorKind::RangeError,
                "BigInt radix must be between 2 and 36",
            ));
        }
        Ok(vm.bigint_text(&value, radix as u32)?.into())
    });
    method(vm, prototype, "toLocaleString", 0, |vm, this, _| {
        let value = receiver(vm, this)?;
        Ok(vm.bigint_text(&value, 10)?.into())
    });
}

impl Vm {
    pub(crate) fn number_constructor_value(&mut self, value: &Value) -> Result<f64, JsError> {
        let primitive = self.coerce_primitive(value.clone(), false)?;
        if let Value::BigInt(value) = primitive {
            self.consume_work(value.words())?;
            Ok(match value.0 {
                Repr::Small(value) => value as f64,
                Repr::High(value) => value as f64,
                Repr::Large(value) => value.to_f64().expect("arbitrary integer converts to f64"),
            })
        } else {
            self.to_number(&primitive)
        }
    }
}

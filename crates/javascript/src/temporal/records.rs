//! Read user property bags in the specified order and build private field records.
use crate::{ErrorKind, JsError, Value, Vm};

fn keys(vm: &mut Vm, array: &Value) -> Result<Vec<String>, JsError> {
    if array.is_undefined() {
        return Ok(Vec::new());
    }
    // These arrays are closed over in the private Temporal realm. Their keys
    // are known field names, selected by the target Temporal brand/calendar.
    let len = vm
        .get_property(array.clone(), "length")?
        .as_number()
        .unwrap_or_default() as usize;
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        let key = vm.get_property(array.clone(), &i.to_string())?;
        result.push(
            key.as_string()
                .ok_or_else(|| JsError::type_error("Invalid Temporal field name"))?
                .to_string_lossy(),
        );
    }
    Ok(result)
}

pub(super) fn select(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let keys = keys(vm, &args[0])?;
    let dest = if let Some(dest) = args.get(2).filter(|v| !v.is_undefined()) {
        dest.clone()
    } else {
        vm.new_object_with_prototype(Value::Null)?
    };
    for key in keys {
        let v = vm.get_property(args[1].clone(), &key)?;
        vm.define_data_property(dest.clone(), &key, v, true, true, true)?;
    }
    Ok(dest)
}

pub(super) fn refine(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let bag = &args[0];
    let names = keys(vm, &args[1])?;
    let required = keys(vm, &args[3])?;
    let disallow_empty = args[4].is_truthy();
    let mut any = false;
    let res = vm.new_object();
    for name in names {
        let value = vm.get_property(bag.clone(), &name)?;
        if value.is_undefined() {
            if required.contains(&name) {
                return Err(JsError::type_error(format!(
                    "Missing Temporal field {name}"
                )));
            }
            continue;
        }
        any = true;
        let refined = match name.as_str() {
            "day" | "month" | "hour" | "minute" | "second" | "millisecond" | "microsecond"
            | "nanosecond" => {
                let n = vm.to_number(&value)?.trunc();
                if !n.is_finite() || (matches!(name.as_str(), "day" | "month") && n <= 0.0) {
                    return Err(JsError::new(
                        ErrorKind::RangeError,
                        format!("Invalid Temporal {name}"),
                    ));
                }
                Value::Number(if n == 0.0 { 0.0 } else { n })
            }
            "era" => Value::String(vm.to_string(&value)?),
            "monthCode" => {
                let v = vm.coerce_primitive(value, true)?;
                if v.as_string().is_none() {
                    return Err(JsError::type_error("monthCode must be a string"));
                }
                v
            }
            // The remaining converters include Temporal time zone resolution
            // and string offset parsing. Invoke them immediately after Get.
            _ => {
                let f = vm.get_property(args[2].clone(), &name)?;
                if f.is_undefined() {
                    value
                } else {
                    vm.call(f, Value::Undefined, &[value, name.clone().into()])?
                }
            }
        };
        vm.define_data_property(res.clone(), &name, refined, true, true, true)?;
    }
    if disallow_empty && !any {
        return Err(JsError::type_error("No valid Temporal fields"));
    }
    Ok(res)
}

pub(super) const CLOCK: [(&str, i64, i64); 6] = [
    ("hour", 23, 3_600_000_000_000),
    ("minute", 59, 60_000_000_000),
    ("second", 59, 1_000_000_000),
    ("millisecond", 999, 1_000_000),
    ("microsecond", 999, 1_000),
    ("nanosecond", 999, 1),
];

pub(super) fn clock(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let source = args[0].clone();
    let reject = args.get(1).and_then(Value::as_number) == Some(1.0);
    let result = vm.new_object();
    for (name, max, _) in CLOCK {
        let v = vm
            .get_property(source.clone(), name)?
            .as_number()
            .unwrap_or_default();
        let bounded = v.clamp(0.0, max as f64);
        if reject && bounded != v {
            return Err(JsError::new(
                ErrorKind::RangeError,
                format!("Invalid Temporal {name}"),
            ));
        }
        vm.define_data_property(result.clone(), name, bounded.into(), true, true, true)?;
    }
    Ok(result)
}

const UNITS: [&str; 10] = [
    "nanosecond",
    "microsecond",
    "millisecond",
    "second",
    "minute",
    "hour",
    "day",
    "week",
    "month",
    "year",
];
const MODES: [&str; 9] = [
    "floor",
    "halfFloor",
    "ceil",
    "halfCeil",
    "trunc",
    "halfTrunc",
    "expand",
    "halfExpand",
    "halfEven",
];

fn invalid_option(key: &str) -> JsError {
    JsError::new(ErrorKind::RangeError, format!("Invalid Temporal {key}"))
}

fn unit_option(vm: &mut Vm, options: &Value, key: &str) -> Result<Option<usize>, JsError> {
    let value = vm.get_property(options.clone(), key)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let text = vm.to_string(&value)?.to_string_lossy();
    if text == "auto" {
        return Ok(None);
    }
    let singular = text.strip_suffix('s').unwrap_or(&text);
    UNITS
        .iter()
        .position(|&u| u == singular)
        .map(Some)
        .ok_or_else(|| invalid_option(key))
}

pub(super) fn difference_options(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let options = &args[1];
    let max = args.get(3).and_then(Value::as_number).unwrap_or(9.0) as usize;
    let min = args.get(4).and_then(Value::as_number).unwrap_or(0.0) as usize;
    let default_largest = args[2].as_number().unwrap_or_default() as usize;
    let (smallest, largest, increment, mut mode) = if options.is_undefined() {
        (min, default_largest, 1, 4)
    } else {
        if options.as_object().is_none() {
            return Err(JsError::type_error("Temporal options must be an object"));
        }
        let largest = unit_option(vm, options, "largestUnit")?;
        let increment = vm.get_property(options.clone(), "roundingIncrement")?;
        let increment = if increment.is_undefined() {
            1
        } else {
            let n = vm.to_number(&increment)?.trunc();
            if !(1.0..=1_000_000_000.0).contains(&n) {
                return Err(invalid_option("roundingIncrement"));
            }
            n as usize
        };
        let mode = vm.get_property(options.clone(), "roundingMode")?;
        let mode = if mode.is_undefined() {
            4
        } else {
            let name = vm.to_string(&mode)?.to_string_lossy();
            MODES
                .iter()
                .position(|&s| s == name)
                .ok_or_else(|| invalid_option("roundingMode"))?
        };
        let smallest = unit_option(vm, options, "smallestUnit")?.unwrap_or(min);
        let largest = largest.unwrap_or(default_largest.max(smallest));
        if largest < min || largest > max || smallest < min || smallest > largest {
            return Err(invalid_option("unit range"));
        }
        // Rounding a time unit cannot carry into its next larger unit.
        if let Some(&radix) = [1000, 1000, 1000, 60, 60, 24].get(smallest)
            && (increment >= radix || radix % increment != 0)
        {
            return Err(invalid_option("roundingIncrement"));
        }
        (smallest, largest, increment, mode)
    };
    if args[0].is_truthy() && mode < 4 {
        mode = (mode + 2) % 4;
    }
    vm.try_new_array(&[
        largest.into(),
        smallest.into(),
        increment.into(),
        mode.into(),
    ])
}

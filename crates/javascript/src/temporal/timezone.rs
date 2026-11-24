//! Use TZDB's recorded transitions rather than finding them by repeatedly
//! formatting instants and binary-searching for an offset change.
use crate::{ErrorKind, JsError, Value, Vm};
use jiff::{
    Timestamp,
    tz::{TimeZone, TimeZoneTransition},
};

use crate::date::calendar::project_timestamp;
const MAX_EPOCH_SECOND: i64 = 100_000_000 * 86_400;

pub(super) fn resolve(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let name = args
        .first()
        .and_then(Value::as_string)
        .ok_or_else(|| JsError::type_error("Expected time zone identifier"))?;
    let (identifier, primary) = crate::intl::named_time_zone(&name.to_string_lossy())
        .ok_or_else(|| JsError::new(ErrorKind::RangeError, "Unknown time zone"))?;
    vm.try_new_array(&[identifier.into(), primary.into()])
}

pub(super) fn open(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let id = args
        .first()
        .and_then(Value::as_string)
        .ok_or_else(|| JsError::type_error("Expected time zone identifier"))?;
    let zone = TimeZone::get(&id.to_string_lossy())
        .map_err(|_| JsError::new(ErrorKind::RangeError, "Unknown time zone"))?;
    Ok(vm.new_native_function("", move |vm, _, args| {
        let second =
            args.first()
                .and_then(Value::as_number)
                .ok_or_else(|| JsError::type_error("Expected epoch second"))? as i64;
        let direction = args.get(1).and_then(Value::as_number).unwrap_or(0.0);
        let fraction = args.get(2).and_then(Value::as_number).unwrap_or(0.0) as i32;
        let (instant, shift) = project_timestamp(second, fraction);
        if direction == 0.0 {
            return Ok(Value::Number(f64::from(zone.to_offset(instant).seconds())));
        }
        let mut transitions: Box<dyn Iterator<Item = TimeZoneTransition<'_>>> = if direction > 0.0 {
            Box::new(zone.following(instant))
        } else {
            Box::new(zone.preceding(instant))
        };
        for transition in &mut transitions {
            vm.tick()?;
            let time = transition.timestamp().as_second();
            let Some(before) = Timestamp::from_second(time - 1).ok() else {
                continue;
            };
            if zone.to_offset(before) == transition.offset() {
                continue; // A designation-only change is not a Temporal transition.
            }
            // Historical transitions retain their actual date when a zone
            // has stopped observing daylight saving time.
            let time = if (time - instant.as_second()).abs() <= 366 * 86_400 {
                time + shift
            } else {
                time
            };
            if (-MAX_EPOCH_SECOND..=MAX_EPOCH_SECOND).contains(&time) {
                return Ok(Value::Number(time as f64));
            }
            break;
        }
        Ok(Value::Undefined)
    }))
}

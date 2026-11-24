//! ECMAScript Date, with an internal UTC millisecond value and host-local
//! timezone rules at the civil-time boundary.
mod calendar;
mod local;
mod parse;

use crate::{
    ErrorKind, JsError, ObjectId, Value, Vm,
    builtins::{constructor, method, replace_construct},
    heap::ObjectKind,
};
use calendar::{CivilTime, MINUTE_MS, MONTHS, WEEKDAYS, make_date, make_full_year, time_clip};
use local::Zone;
use std::{
    ops::Range,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

fn now() -> f64 {
    let time = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as f64,
        Err(error) => -(error.duration().as_millis() as f64),
    };
    time_clip(time)
}

fn receiver(vm: &Vm, value: &Value) -> Result<(ObjectId, f64), JsError> {
    if let Some(id) = value.as_object()
        && let ObjectKind::Date(time) = vm.heap.object(id)?.kind
    {
        return Ok((id, time));
    }
    Err(JsError::type_error("Receiver has no Date internal slot"))
}

fn update(vm: &mut Vm, id: ObjectId, time: f64) -> Result<Value, JsError> {
    vm.heap.object_mut(id)?.kind = ObjectKind::Date(time);
    Ok(time.into())
}

fn make_from_arguments(vm: &mut Vm, args: &[Value]) -> Result<f64, JsError> {
    let mut fields = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (field, value) in fields.iter_mut().zip(args) {
        *field = vm.to_number(value)?;
    }
    fields[0] = make_full_year(fields[0]);
    Ok(make_date(fields))
}

fn constructor_time(vm: &mut Vm, args: &[Value]) -> Result<f64, JsError> {
    match args {
        [] => Ok(now()),
        [value] => {
            if let Some(id) = value.as_object()
                && let ObjectKind::Date(time) = vm.heap.object(id)?.kind
            {
                return Ok(time);
            }
            let primitive = vm.primitive_hint(value.clone(), "default")?;
            let time = if let Value::String(text) = primitive {
                parse::parse(vm, &text)?
            } else {
                vm.to_number(&primitive)?
            };
            Ok(time_clip(time))
        }
        _ => {
            let time = make_from_arguments(vm, args)?;
            Ok(time_clip(local::to_utc(time)))
        }
    }
}

fn year_text(year: i64) -> String {
    if year < 0 {
        format!("-{:04}", year.abs())
    } else {
        format!("{year:04}")
    }
}

fn iso_year(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", year.abs())
    } else {
        format!("+{year:06}")
    }
}

fn iso_string(time: f64) -> Result<Value, JsError> {
    if time.is_nan() {
        return Err(JsError::new(ErrorKind::RangeError, "Invalid Date"));
    }
    let date = CivilTime::from_epoch(time as i64);
    Ok(format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        iso_year(date.year),
        date.month,
        date.day,
        date.hour,
        date.minute,
        date.second,
        date.millisecond
    )
    .into())
}

fn rfc_string(time: f64) -> Value {
    if time.is_nan() {
        return "Invalid Date".into();
    }
    let date = CivilTime::from_epoch(time as i64);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[date.weekday as usize],
        date.day,
        MONTHS[(date.month - 1) as usize],
        year_text(date.year),
        date.hour,
        date.minute,
        date.second
    )
    .into()
}

#[derive(Clone, Copy)]
enum LocalFormat {
    Date,
    Time,
    Full,
}

fn local_string(time: f64, format: LocalFormat) -> Value {
    if time.is_nan() {
        return "Invalid Date".into();
    }
    let zone = Zone::system();
    let utc = time as i64;
    let offset = zone.offset_at(utc);
    let civil = CivilTime::from_epoch(utc + offset);
    let date = || {
        format!(
            "{} {} {:02} {}",
            WEEKDAYS[civil.weekday as usize],
            MONTHS[(civil.month - 1) as usize],
            civil.day,
            year_text(civil.year)
        )
    };
    let clock = || {
        let minutes = offset.abs() / MINUTE_MS;
        let sign = if offset < 0 { '-' } else { '+' };
        let name = zone.name_at(utc, offset);
        format!(
            "{:02}:{:02}:{:02} GMT{sign}{:02}{:02} ({name})",
            civil.hour,
            civil.minute,
            civil.second,
            minutes / 60,
            minutes % 60
        )
    };
    match format {
        LocalFormat::Date => date().into(),
        LocalFormat::Time => clock().into(),
        LocalFormat::Full => format!("{} {}", date(), clock()).into(),
    }
}

fn get_component(
    vm: &Vm,
    this: &Value,
    local: bool,
    component: impl FnOnce(CivilTime) -> f64,
) -> Result<Value, JsError> {
    let (_, time) = receiver(vm, this)?;
    if time.is_nan() {
        return Ok(time.into());
    }
    let time = time as i64;
    let local = if local {
        time + Zone::system().offset_at(time)
    } else {
        time
    };
    Ok(component(CivilTime::from_epoch(local)).into())
}

#[derive(Clone, Copy)]
enum Component {
    FullYear,
    LegacyYear,
    Month,
    Date,
    Hours,
    Minutes,
    Seconds,
    Milliseconds,
}

impl Component {
    fn fields(self) -> Range<usize> {
        match self {
            Self::FullYear => 0..3,
            Self::LegacyYear => 0..1,
            Self::Month => 1..3,
            Self::Date => 2..3,
            Self::Hours => 3..7,
            Self::Minutes => 4..7,
            Self::Seconds => 5..7,
            Self::Milliseconds => 6..7,
        }
    }
}

fn set_component(
    vm: &mut Vm,
    this: &Value,
    args: &[Value],
    component: Component,
    local: bool,
) -> Result<Value, JsError> {
    let (id, snapshot) = receiver(vm, this)?;
    let fields = component.fields();
    let mut supplied = [None; 7];
    for (index, field) in fields.enumerate() {
        if let Some(argument) = args.get(index) {
            supplied[field] = Some(vm.to_number(argument)?);
        } else if index == 0 {
            supplied[field] = Some(f64::NAN);
        }
    }
    if snapshot.is_nan() && !matches!(component, Component::FullYear | Component::LegacyYear) {
        // Preserve any writes performed by argument coercion before returning.
        return Ok(f64::NAN.into());
    }
    let zone = local.then(Zone::system);
    let base = if snapshot.is_nan() {
        0
    } else {
        let base = snapshot as i64;
        zone.as_ref()
            .map_or(base, |zone| base + zone.offset_at(base))
    };
    let mut values = CivilTime::from_epoch(base).numbers();
    for (value, supplied) in values.iter_mut().zip(supplied) {
        if let Some(supplied) = supplied {
            *value = supplied;
        }
    }
    if matches!(component, Component::LegacyYear) {
        values[0] = make_full_year(values[0]);
    }
    let mut time = make_date(values);
    if let Some(zone) = zone {
        if time.is_finite() && time.abs() <= calendar::CLIP_LIMIT + calendar::DAY_MS as f64 {
            time = zone.utc_from_wall(time as i64) as f64;
        } else {
            time = f64::NAN;
        }
    }
    update(vm, id, time_clip(time))
}

pub(crate) fn install(vm: &mut Vm) {
    let prototype = vm.new_object();
    let intrinsic = prototype.as_object().unwrap();
    vm.root(prototype.clone());
    vm.realm.clone_intrinsics.date = Some(intrinsic);
    let date = constructor(vm, "Date", 7, prototype.clone(), |_, _, _| {
        Ok(local_string(now(), LocalFormat::Full))
    });
    replace_construct(vm, date.clone(), move |vm, _, args, target| {
        let time = constructor_time(vm, args)?;
        let prototype = vm.prototype_from_constructor(target, intrinsic)?;
        vm.allocate_constructed(ObjectKind::Date(time), prototype)
    });
    method(vm, date.clone(), "now", 0, |_, _, _| Ok(now().into()));
    method(vm, date.clone(), "parse", 1, |vm, _, args| {
        let text = vm.to_string(args.first().unwrap_or(&Value::Undefined))?;
        Ok(parse::parse(vm, &text)?.into())
    });
    method(vm, date, "UTC", 7, |vm, _, args| {
        Ok(time_clip(make_from_arguments(vm, args)?).into())
    });
    let primitive = vm.native(
        "[Symbol.toPrimitive]",
        1,
        false,
        Rc::new(|vm, this, args| {
            if this.as_object().is_none() {
                return Err(JsError::type_error(
                    "Date primitive conversion requires an object",
                ));
            }
            let hint = args.first().and_then(Value::as_string);
            if hint.is_some_and(|hint| hint == &"number".into()) {
                vm.ordinary_primitive(this, false)
            } else if hint.is_some_and(|hint| hint == &"string".into() || hint == &"default".into())
            {
                vm.ordinary_primitive(this, true)
            } else {
                Err(JsError::type_error("Invalid Date conversion hint"))
            }
        }),
    );
    vm.define_data_property_key(
        prototype.clone(),
        vm.well_known_key("toPrimitive"),
        primitive,
        false,
        false,
        true,
    )
    .unwrap();

    for (local, utc, read) in [
        (
            "getFullYear",
            "getUTCFullYear",
            (|t: CivilTime| t.year as f64) as fn(CivilTime) -> f64,
        ),
        ("getMonth", "getUTCMonth", |t| (t.month - 1) as f64),
        ("getDate", "getUTCDate", |t| t.day as f64),
        ("getDay", "getUTCDay", |t| t.weekday as f64),
        ("getHours", "getUTCHours", |t| t.hour as f64),
        ("getMinutes", "getUTCMinutes", |t| t.minute as f64),
        ("getSeconds", "getUTCSeconds", |t| t.second as f64),
        ("getMilliseconds", "getUTCMilliseconds", |t| {
            t.millisecond as f64
        }),
    ] {
        method(vm, prototype.clone(), local, 0, move |vm, this, _| {
            get_component(vm, &this, true, read)
        });
        method(vm, prototype.clone(), utc, 0, move |vm, this, _| {
            get_component(vm, &this, false, read)
        });
    }
    method(vm, prototype.clone(), "getYear", 0, |vm, this, _| {
        get_component(vm, &this, true, |t| (t.year - 1900) as f64)
    });
    for name in ["getTime", "valueOf"] {
        method(vm, prototype.clone(), name, 0, |vm, this, _| {
            Ok(receiver(vm, &this)?.1.into())
        });
    }
    method(
        vm,
        prototype.clone(),
        "getTimezoneOffset",
        0,
        |vm, this, _| {
            let (_, time) = receiver(vm, &this)?;
            if time.is_nan() {
                return Ok(time.into());
            }
            Ok((-(Zone::system().offset_at(time as i64)) as f64 / MINUTE_MS as f64).into())
        },
    );
    for (local, utc, component) in [
        ("setFullYear", "setUTCFullYear", Component::FullYear),
        ("setMonth", "setUTCMonth", Component::Month),
        ("setDate", "setUTCDate", Component::Date),
        ("setHours", "setUTCHours", Component::Hours),
        ("setMinutes", "setUTCMinutes", Component::Minutes),
        ("setSeconds", "setUTCSeconds", Component::Seconds),
        (
            "setMilliseconds",
            "setUTCMilliseconds",
            Component::Milliseconds,
        ),
    ] {
        let length = component.fields().len();
        method(
            vm,
            prototype.clone(),
            local,
            length,
            move |vm, this, args| set_component(vm, &this, args, component, true),
        );
        method(vm, prototype.clone(), utc, length, move |vm, this, args| {
            set_component(vm, &this, args, component, false)
        });
    }
    method(vm, prototype.clone(), "setYear", 1, |vm, this, args| {
        set_component(vm, &this, args, Component::LegacyYear, true)
    });
    method(vm, prototype.clone(), "setTime", 1, |vm, this, args| {
        let (id, _) = receiver(vm, &this)?;
        let time = vm.to_number(args.first().unwrap_or(&Value::Undefined))?;
        update(vm, id, time_clip(time))
    });
    method(vm, prototype.clone(), "toISOString", 0, |vm, this, _| {
        iso_string(receiver(vm, &this)?.1)
    });
    method(vm, prototype.clone(), "toJSON", 1, |vm, this, _| {
        let object = vm.coerce_object(this)?;
        let primitive = vm.primitive_hint(object.clone(), "number")?;
        if primitive
            .as_number()
            .is_some_and(|value| !value.is_finite())
        {
            return Ok(Value::Null);
        }
        let to_iso = vm.get_property(object.clone(), "toISOString")?;
        vm.invoke(to_iso, object, &[])
    });
    let utc = method(vm, prototype.clone(), "toUTCString", 0, |vm, this, _| {
        Ok(rfc_string(receiver(vm, &this)?.1))
    });
    vm.define_data_property(prototype.clone(), "toGMTString", utc, true, false, true)
        .unwrap();
    for (name, format) in [
        ("toString", LocalFormat::Full),
        ("toDateString", LocalFormat::Date),
        ("toTimeString", LocalFormat::Time),
        ("toLocaleString", LocalFormat::Full),
        ("toLocaleDateString", LocalFormat::Date),
        ("toLocaleTimeString", LocalFormat::Time),
    ] {
        method(vm, prototype.clone(), name, 0, move |vm, this, _| {
            Ok(local_string(receiver(vm, &this)?.1, format))
        });
    }
}

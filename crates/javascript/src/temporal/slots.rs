//! Temporal internal slots are branded on the instance and traced by the VM.
//! Applying a method from another realm recognizes the same brand, while its
//! result, calendar operations and exceptions use that method's own realm.
use crate::{JsError, Value, Vm};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    ZonedDateTime,
    Instant,
    PlainMonthDay,
    PlainYearMonth,
    PlainTime,
    PlainDateTime,
    PlainDate,
    Duration,
}
impl Kind {
    fn parse(name: &Value) -> Option<Self> {
        let name = name.as_string()?;
        [
            ("Instant", Self::Instant),
            ("Duration", Self::Duration),
            ("PlainDate", Self::PlainDate),
            ("PlainTime", Self::PlainTime),
            ("PlainDateTime", Self::PlainDateTime),
            ("ZonedDateTime", Self::ZonedDateTime),
            ("PlainMonthDay", Self::PlainMonthDay),
            ("PlainYearMonth", Self::PlainYearMonth),
        ]
        .into_iter()
        .find(|(s, _)| name.as_utf16().iter().copied().eq(s.encode_utf16()))
        .map(|(_, k)| k)
    }
}
struct Instance {
    kind: Kind,
    owner: Rc<()>,
}

pub(super) fn intrinsics(vm: &mut Vm) -> [Value; 3] {
    let owner = Rc::new(());
    let caller = owner.clone();
    let read = vm.new_native_function("", move |vm, _, args| {
        let Some([obj, kind, adopt]) = args.get(..3) else {
            return Ok(Value::Undefined);
        };
        let Some(instance) = vm.host_data::<Instance>(obj) else {
            return Ok(Value::Undefined);
        };
        if Kind::parse(kind) != Some(instance.kind) {
            return Ok(Value::Undefined);
        }
        let slots = vm.host_slot(obj, 0).expect("branded Temporal slots");
        if Rc::ptr_eq(&instance.owner, &caller) {
            return Ok(slots);
        }
        vm.call(adopt.clone(), Value::Undefined, &[slots])
    });
    let write = vm.new_native_function("", move |vm, _, args| {
        let Some([obj, kind, slots, formatter]) = args.get(..4) else {
            return Err(JsError::type_error("Missing Temporal slots"));
        };
        let kind =
            Kind::parse(kind).ok_or_else(|| JsError::type_error("Invalid Temporal brand"))?;
        vm.reserve_allocation(std::mem::size_of::<Instance>())?;
        vm.set_host_slot(obj.clone(), 0, slots.clone())?;
        vm.set_host_slot(obj.clone(), 1, formatter.clone())?;
        vm.set_host_data(
            obj.clone(),
            Rc::new(Instance {
                kind,
                owner: owner.clone(),
            }),
        )?;
        Ok(obj.clone())
    });
    // A getter retains its two private algorithms in GC-visible host slots.
    let make_field_getter = vm.new_native_function("", |vm, _, args| {
        let kind = args
            .first()
            .and_then(Kind::parse)
            .ok_or_else(|| JsError::type_error("Invalid Temporal accessor"))?;
        let field = args[1].as_string().unwrap().to_string_lossy();
        let read_slots = args[2].clone();
        let calculate = args[3].clone();
        let get_slots = read_slots.clone();
        let compute = calculate.clone();
        let getter = vm.new_native_function(&format!("get {field}"), move |vm, receiver, _| {
            if !vm
                .host_data::<Instance>(&receiver)
                .is_some_and(|data| data.kind == kind)
            {
                return Err(JsError::type_error("Invalid Temporal receiver"));
            }
            let slots = vm.host_slot(&receiver, 0).expect("branded Temporal slots");
            if field == "timeZoneId" {
                let zone = vm.get_property(slots, "timeZone")?;
                return vm.get_property(zone, "id");
            }
            if field == "calendarId" {
                return match vm.get_property(slots, "calendar")? {
                    Value::Undefined => Ok("iso8601".into()),
                    Value::Number(_) => Ok("gregory".into()),
                    cal => vm.get_property(cal, "id"),
                };
            }
            let civil = if kind == Kind::ZonedDateTime {
                // Zoned dates need local civil fields for all field accessors.
                // The zone and epoch are immutable, including across realms.
                if let Some(fields) = vm.host_slot(&receiver, 2) {
                    fields
                } else {
                    let fields = vm.call(
                        get_slots.clone(),
                        Value::Undefined,
                        std::slice::from_ref(&receiver),
                    )?;
                    vm.set_host_slot(receiver.clone(), 2, fields.clone())?;
                    fields
                }
            } else {
                slots
            };
            if kind == Kind::Duration
                || matches!(
                    field.as_str(),
                    "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
                )
            {
                return vm.get_property(civil, &field);
            }
            if matches!(
                field.as_str(),
                "year" | "month" | "day" | "monthCode" | "era" | "eraYear"
            ) {
                let calendar = vm.get_property(civil.clone(), "calendar")?;
                if !matches!(calendar, Value::Object(_)) {
                    return match field.as_str() {
                        "monthCode" => {
                            let month = vm.get_property(civil, "month")?.as_number().unwrap();
                            Ok(format!("M{:02}", month as u8).into())
                        }
                        "era" | "eraYear" if calendar.is_undefined() => Ok(Value::Undefined),
                        "era" | "eraYear" => {
                            let year = vm.get_property(civil, "year")?.as_number().unwrap();
                            if field == "era" {
                                Ok(if year < 1.0 { "bce" } else { "ce" }.into())
                            } else {
                                Ok(Value::Number(if year < 1.0 { 1.0 - year } else { year }))
                            }
                        }
                        _ => vm.get_property(civil, &field),
                    };
                }
            }
            if matches!(
                field.as_str(),
                "year"
                    | "month"
                    | "day"
                    | "monthCode"
                    | "inLeapYear"
                    | "daysInMonth"
                    | "monthsInYear"
                    | "daysInYear"
                    | "dayOfYear"
            ) && let Some(value) = super::calendar::derived_field(vm, &civil, &field)?
            {
                return Ok(value);
            }
            vm.call(compute.clone(), Value::Undefined, &[civil])
        });
        vm.set_host_slot(getter.clone(), 0, read_slots)?;
        vm.set_host_slot(getter.clone(), 1, calculate)?;
        Ok(getter)
    });
    [read, write, make_field_getter]
}

pub(crate) fn intl_fields(
    vm: &mut Vm,
    value: &Value,
) -> Result<Option<(Kind, String, i64)>, JsError> {
    let Some(instance) = vm.host_data::<Instance>(value) else {
        return Ok(None);
    };
    let kind = instance.kind;
    let label = match kind {
        Kind::ZonedDateTime | Kind::Duration => {
            return Err(JsError::type_error(
                "This Temporal type cannot be formatted by Intl.DateTimeFormat",
            ));
        }
        Kind::Instant => "Instant",
        Kind::PlainDate => "PlainDate",
        Kind::PlainDateTime => "PlainDateTime",
        Kind::PlainMonthDay => "PlainMonthDay",
        Kind::PlainYearMonth => "PlainYearMonth",
        Kind::PlainTime => "PlainTime",
    };
    let slots = vm.host_slot(value, 0).expect("branded Temporal slots");
    let extractor = vm
        .host_slot(value, 1)
        .expect("Temporal calendar conversion");
    let parts = vm.call(extractor, Value::Undefined, &[label.into(), slots])?;
    let cal = vm
        .get_property(parts.clone(), "0")?
        .as_string()
        .unwrap()
        .to_string_lossy();
    let ms = vm.get_property(parts, "1")?.as_number().unwrap() as i64;
    Ok(Some((kind, cal, ms)))
}

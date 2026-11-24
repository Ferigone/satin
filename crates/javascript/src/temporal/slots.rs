//! Temporal internal slots are branded on the instance and traced by the VM.
//! Applying a method from another realm recognizes the same brand, while its
//! result, calendar operations and exceptions use that method's own realm.
use crate::{JsError, Value, Vm};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    ZonedDateTime, Instant, PlainMonthDay, PlainYearMonth,
    PlainTime, PlainDateTime, PlainDate, Duration,
}
impl Kind {
    fn parse(name: &Value) -> Option<Self> {
        let name = name.as_string()?;
        [
            ("Instant", Self::Instant), ("Duration", Self::Duration),
            ("PlainDate", Self::PlainDate), ("PlainTime", Self::PlainTime),
            ("PlainDateTime", Self::PlainDateTime), ("ZonedDateTime", Self::ZonedDateTime),
            ("PlainMonthDay", Self::PlainMonthDay), ("PlainYearMonth", Self::PlainYearMonth),
        ].into_iter().find(|(s,_)|name.as_utf16().iter().copied().eq(s.encode_utf16())).map(|(_,k)|k)
    }
}
struct Instance {
    kind: Kind,
    owner: Rc<()>,
}

pub(super) fn intrinsics(vm: &mut Vm) -> [Value; 2] {
    let owner = Rc::new(());
    let caller = owner.clone();
    let read = vm.new_native_function("", move |vm, _, args| {
        let Some([obj, kind, adopt]) = args.get(..3) else { return Ok(Value::Undefined) };
        let Some(instance) = vm.host_data::<Instance>(obj) else { return Ok(Value::Undefined) };
        if Kind::parse(kind) != Some(instance.kind) { return Ok(Value::Undefined); }
        let slots = vm.host_slot(obj, 0).expect("branded Temporal slots");
        if Rc::ptr_eq(&instance.owner, &caller) { return Ok(slots); }
        vm.call(adopt.clone(), Value::Undefined, &[slots])
    });
    let write = vm.new_native_function("", move |vm, _, args| {
        let Some([obj, kind, slots, formatter]) = args.get(..4) else {
            return Err(JsError::type_error("Missing Temporal slots"));
        };
        let kind = Kind::parse(kind).ok_or_else(|| JsError::type_error("Invalid Temporal brand"))?;
        vm.reserve_allocation(std::mem::size_of::<Instance>())?;
        vm.set_host_slot(obj.clone(), 0, slots.clone())?;
        vm.set_host_slot(obj.clone(), 1, formatter.clone())?;
        vm.set_host_data(obj.clone(), Rc::new(Instance {kind, owner:owner.clone()}))?;
        Ok(obj.clone())
    });
    [read, write]
}

pub(crate) fn intl_fields(vm: &mut Vm, value: &Value) -> Result<Option<(Kind, String, i64)>, JsError> {
    let Some(instance) = vm.host_data::<Instance>(value) else { return Ok(None) };
    let kind = instance.kind;
    let label = match kind {
        Kind::ZonedDateTime | Kind::Duration => return Err(JsError::type_error(
            "This Temporal type cannot be formatted by Intl.DateTimeFormat",
        )),
        Kind::Instant => "Instant",
        Kind::PlainDate => "PlainDate",
        Kind::PlainDateTime => "PlainDateTime",
        Kind::PlainMonthDay => "PlainMonthDay",
        Kind::PlainYearMonth => "PlainYearMonth",
        Kind::PlainTime => "PlainTime",
    };
    let slots = vm.host_slot(value, 0).expect("branded Temporal slots");
    let extractor = vm.host_slot(value, 1).expect("Temporal calendar conversion");
    let parts = vm.call(extractor, Value::Undefined, &[label.into(), slots])?;
    let cal = vm.get_property(parts.clone(), "0")?.as_string().unwrap().to_string_lossy();
    let ms = vm.get_property(parts, "1")?.as_number().unwrap() as i64;
    Ok(Some((kind, cal, ms)))
}

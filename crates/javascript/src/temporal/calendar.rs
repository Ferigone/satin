//! Unlocalized fields for calendars which use ICU's lunisolar / observational
//! data. Reading the calendar directly avoids inferring months from translated
//! display strings or stepping backwards through a year of formatted dates.
use crate::{ErrorKind, JsError, Value, Vm};
use icu_calendar::{AnyCalendar, AnyCalendarKind, Date, types::RataDie};

fn calendar(args: &[Value]) -> Result<AnyCalendar, JsError> {
    let name = args
        .first()
        .and_then(Value::as_string)
        .map(|s| s.to_string_lossy());
    let kind = match name.as_deref() {
        Some("chinese") => AnyCalendarKind::Chinese,
        Some("dangi") => AnyCalendarKind::Dangi,
        Some("islamic-umalqura") => AnyCalendarKind::HijriUmmAlQura,
        _ => return Err(invalid_date()),
    };
    Ok(AnyCalendar::new(kind))
}

fn integer(args: &[Value], index: usize) -> Result<i32, JsError> {
    let n = args
        .get(index)
        .and_then(Value::as_number)
        .ok_or_else(invalid_date)?;
    if n.fract() != 0.0 || !(-300_000.0..=300_000.0).contains(&n) {
        return Err(invalid_date());
    }
    Ok(n as i32)
}

fn invalid_date() -> JsError {
    JsError::new(
        ErrorKind::RangeError,
        "Calendar date outside the supported range",
    )
}

pub(super) fn fields(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    let year = integer(args, 1)?;
    let month = u8::try_from(integer(args, 2)?).map_err(|_| invalid_date())?;
    let day = u8::try_from(integer(args, 3)?).map_err(|_| invalid_date())?;
    let date = Date::try_new_iso(year, month, day)
        .map_err(|_| invalid_date())?
        .to_calendar(calendar(args)?);
    let result = vm.new_object();
    for (key, value) in [
        ("year", date.year().extended_year()),
        ("month", i32::from(date.month().ordinal)),
        ("day", i32::from(date.day_of_month().0)),
    ] {
        vm.define_data_property(
            result.clone(),
            key,
            Value::Number(f64::from(value)),
            true,
            true,
            true,
        )?;
    }
    Ok(result)
}

pub(super) fn year(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    const UNIX_EPOCH_DAY: i64 = 719_163;
    let cal = calendar(args)?;
    let year = integer(args, 1)?;
    let mut date = Date::try_new(year.into(), 1.into(), 1, icu_calendar::Ref(&cal))
        .map_err(|_| invalid_date())?;
    let mut starts = Vec::new();
    let mut numbers = Vec::new();
    for _ in 0..date.months_in_year() {
        vm.tick()?;
        let start = date.to_rata_die().to_i64_date();
        starts.push(Value::Number(
            ((start - UNIX_EPOCH_DAY) * 86_400_000) as f64,
        ));
        // A leap month repeats the preceding regular month's number.
        numbers.push(Value::Number(f64::from(date.month().number())));
        let next = RataDie::new(start + i64::from(date.days_in_month()));
        date = Date::from_rata_die(next, icu_calendar::Ref(&cal));
    }
    let result = vm.new_object();
    let starts = vm.try_new_array(&starts)?;
    vm.define_data_property(result.clone(), "V", starts, true, true, true)?;
    let numbers = vm.try_new_array(&numbers)?;
    vm.define_data_property(result.clone(), "ue", numbers, true, true, true)?;
    Ok(result)
}

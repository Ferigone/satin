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
        Some("iso8601" | "gregory" | "japanese" | "roc" | "buddhist") => AnyCalendarKind::Iso,
        Some("hebrew") => AnyCalendarKind::Hebrew,
        Some("coptic") => AnyCalendarKind::Coptic,
        Some("ethiopic" | "ethioaa") => AnyCalendarKind::Ethiopian,
        Some("indian") => AnyCalendarKind::Indian,
        Some("persian") => AnyCalendarKind::Persian,
        Some("islamic-civil") => AnyCalendarKind::HijriTabularTypeIIFriday,
        Some("islamic-tbla") => AnyCalendarKind::HijriTabularTypeIIThursday,
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

const UNIX_EPOCH: i64 = 719_163;

fn iso_record(vm: &mut Vm, day: i64) -> Result<Value, JsError> {
    let date = Date::from_rata_die(RataDie::new(day), icu_calendar::Iso);
    let result = vm.new_object();
    for (key, number) in [
        ("year", date.year().extended_year()),
        ("month", i32::from(date.month().ordinal)),
        ("day", i32::from(date.day_of_month().0)),
    ] {
        vm.define_data_property(result.clone(), key, number.into(), true, true, true)?;
    }
    Ok(result)
}

// ICU's calendar arithmetic is used within its civil range. Very large
// lunisolar month deltas take the interpreter's interruptible path.
pub(super) fn add(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_calendar::{options::{DateAddOptions, Overflow}, types::DateDuration};
    let number = |i: usize| args.get(i).and_then(Value::as_number).ok_or_else(invalid_date);
    let epoch_days = number(1)?;
    let years = number(2)?;
    let months = number(3)?;
    if epoch_days.abs() > 2_000_000.0 || years.abs() > 200.0 || months.abs() > 2400.0 ||
        years * months < 0.0 {
        return Ok(Value::Undefined);
    }
    let cal = calendar(args)?;
    // Advancing across lunisolar months may inspect each intervening year.
    vm.charge_work(1 + months.abs() as usize / 12)?;
    let start = Date::from_rata_die(RataDie::new(epoch_days as i64 + UNIX_EPOCH), cal);
    let mut delta = DateDuration::default();
    delta.is_negative = years < 0.0 || months < 0.0;
    delta.years = years.abs() as u32;
    delta.months = months.abs() as u32;
    let mut options = DateAddOptions::default();
    options.overflow = Some(if number(4)? == 0.0 { Overflow::Constrain } else { Overflow::Reject });
    let end = start.try_added_with_options(delta, options).map_err(|_| invalid_date())?;
    iso_record(vm, end.to_rata_die().to_i64_date())
}

pub(super) fn difference(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_calendar::options::{DateDifferenceOptions, DateDurationUnit};
    let number = |i: usize| args.get(i).and_then(Value::as_number).ok_or_else(invalid_date);
    let a = number(1)?;
    let b = number(2)?;
    if a.abs() > 2_000_000.0 || b.abs() > 2_000_000.0 || (a - b).abs() > 73050.0 {
        return Ok(Value::Undefined);
    }
    vm.charge_work(1 + (a - b).abs() as usize / 365)?;
    let cal = calendar(args)?;
    let from = Date::from_rata_die(RataDie::new(a as i64 + UNIX_EPOCH), icu_calendar::Ref(&cal));
    let to = Date::from_rata_die(RataDie::new(b as i64 + UNIX_EPOCH), icu_calendar::Ref(&cal));
    let mut options = DateDifferenceOptions::default();
    options.largest_unit = Some(match number(3)? as u8 {
        9 => DateDurationUnit::Years, 8 => DateDurationUnit::Months,
        7 => DateDurationUnit::Weeks, _ => DateDurationUnit::Days,
    });
    let fields = from.try_until_with_options(&to, options).map_err(|_| invalid_date())?;
    let sign: f64 = if fields.is_negative { -1.0 } else { 1.0 };
    let result = vm.new_object();
    for (key, n) in [("years",fields.years),("months",fields.months),("weeks",fields.weeks),("days",fields.days)] {
        let n = if n == 0 { 0.0 } else { sign * f64::from(n) };
        vm.define_data_property(result.clone(), key, n.into(), true, true, true)?;
    }
    Ok(result)
}

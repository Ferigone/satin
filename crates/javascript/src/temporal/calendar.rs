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
    let cal = calendar(args)?;
    let values = if let Some(name) = outside_icu_lunisolar_range(&cal, year) {
        vm.charge_work(32)?;
        let epoch = (crate::date::calendar::make_date([
            year as f64,
            f64::from(month) - 1.0,
            f64::from(day),
            0.0,
            0.0,
            0.0,
            0.0,
        ]) / 86_400_000.0) as i64;
        let mut lunar_year = year;
        let mut months = super::lunisolar::months(name, year);
        if epoch < months[0].0 {
            lunar_year -= 1;
            months = super::lunisolar::months(name, lunar_year);
        }
        let i = months.partition_point(|&(start, _)| start <= epoch) - 1;
        [lunar_year, 1 + i as i32, (epoch - months[i].0 + 1) as i32]
    } else {
        let date = Date::try_new_iso(year, month, day)
            .map_err(|_| invalid_date())?
            .to_calendar(cal);
        [
            date.year().extended_year(),
            i32::from(date.month().ordinal),
            i32::from(date.day_of_month().0),
        ]
    };
    let result = vm.new_object();
    for (key, value) in ["year", "month", "day"].into_iter().zip(values) {
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
    let mut starts = Vec::new();
    let mut numbers = Vec::new();
    if let Some(name) = outside_icu_lunisolar_range(&cal, year) {
        vm.charge_work(16)?;
        for (start, code) in super::lunisolar::months(name, year) {
            starts.push(Value::Number((start * 86_400_000) as f64));
            numbers.push(Value::Number(f64::from(code)));
        }
    } else {
        let mut date = Date::try_new(year.into(), 1.into(), 1, icu_calendar::Ref(&cal))
            .map_err(|_| invalid_date())?;
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
    }
    let result = vm.new_object();
    let starts = vm.try_new_array(&starts)?;
    vm.define_data_property(result.clone(), "V", starts, true, true, true)?;
    let numbers = vm.try_new_array(&numbers)?;
    vm.define_data_property(result.clone(), "ue", numbers, true, true, true)?;
    Ok(result)
}

fn outside_icu_lunisolar_range(cal: &AnyCalendar, year: i32) -> Option<&'static str> {
    if (-9000..9000).contains(&year) {
        return None;
    }
    match cal.kind() {
        AnyCalendarKind::Chinese => Some("chinese"),
        AnyCalendarKind::Dangi => Some("dangi"),
        _ => None,
    }
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
    let number = |i: usize| {
        args.get(i)
            .and_then(Value::as_number)
            .ok_or_else(invalid_date)
    };
    let epoch_days = number(1)?;
    let years = number(2)?;
    let months = number(3)?;
    match add_years_months(vm, args, epoch_days, years, months, number(4)? != 0.0)? {
        Some(day) => iso_record(vm, day + UNIX_EPOCH),
        None => Ok(Value::Undefined),
    }
}

fn add_years_months(
    vm: &mut Vm,
    args: &[Value],
    epoch_days: f64,
    years: f64,
    months: f64,
    reject: bool,
) -> Result<Option<i64>, JsError> {
    use icu_calendar::{
        options::{DateAddOptions, Overflow},
        types::DateDuration,
    };
    if epoch_days.abs() > 2_000_000.0
        || years.abs() > 200.0
        || months.abs() > 2400.0
        || years * months < 0.0
    {
        return Ok(None);
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
    options.overflow = Some(if !reject {
        Overflow::Constrain
    } else {
        Overflow::Reject
    });
    let end = start
        .try_added_with_options(delta, options)
        .map_err(|_| invalid_date())?;
    Ok(Some(end.to_rata_die().to_i64_date() - UNIX_EPOCH))
}

pub(super) fn difference(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_calendar::options::{DateDifferenceOptions, DateDurationUnit};
    let number = |i: usize| {
        args.get(i)
            .and_then(Value::as_number)
            .ok_or_else(invalid_date)
    };
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
        9 => DateDurationUnit::Years,
        8 => DateDurationUnit::Months,
        7 => DateDurationUnit::Weeks,
        _ => DateDurationUnit::Days,
    });
    let fields = from
        .try_until_with_options(&to, options)
        .map_err(|_| invalid_date())?;
    let sign: f64 = if fields.is_negative { -1.0 } else { 1.0 };
    let result = vm.new_object();
    for (key, n) in [
        ("years", fields.years),
        ("months", fields.months),
        ("weeks", fields.weeks),
        ("days", fields.days),
    ] {
        let n = if n == 0 { 0.0 } else { sign * f64::from(n) };
        vm.define_data_property(result.clone(), key, n.into(), true, true, true)?;
    }
    Ok(result)
}

// At this point year/era and the month code have already been refined in the
// observable order. ICU can resolve a valid calendar month/day directly.
// Values requiring overflow handling use the calendar's full resolution path.
pub(super) fn from_fields(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_calendar::types::{DateFields, Month};
    let name = args[0]
        .as_string()
        .ok_or_else(invalid_date)?
        .to_string_lossy();
    use icu_calendar::options::{DateFromFieldsOptions, MissingFieldsStrategy, Overflow};
    let mode = args.get(4).and_then(Value::as_number).unwrap_or_default() as u8;
    let mut fields = DateFields::default();
    if let Some(mut year) = args[2].as_number() {
        if !(-7000.0..=7000.0).contains(&year)
            || (name == "islamic-umalqura" && !(1300.0..=1500.0).contains(&year))
            || (name == "persian" && !(1250.0..=1480.0).contains(&year))
        {
            return Ok(Value::Undefined);
        }
        match name.as_str() {
            "buddhist" => year -= 543.0,
            "roc" => year += 1911.0,
            "ethioaa" => year -= 5500.0,
            _ => {}
        }
        fields.extended_year = Some(year as i32);
    }
    let mut options = DateFromFieldsOptions::default();
    if mode != 0 {
        options.missing_fields_strategy = Some(MissingFieldsStrategy::Ecma);
    }
    options.overflow = Some(if args.get(5).and_then(Value::as_number) == Some(1.0) {
        Overflow::Reject
    } else {
        Overflow::Constrain
    });
    let bag = args[1].clone();
    let day = vm
        .get_property(bag.clone(), "day")?
        .as_number()
        .unwrap_or_default();
    if mode != 1 && !(1.0..=31.0).contains(&day) {
        return Ok(Value::Undefined);
    }
    fields.day = Some(if mode == 1 { 1 } else { day as u8 });
    if let Some(month) = vm.get_property(bag.clone(), "month")?.as_number() {
        if !(1.0..=13.0).contains(&month) {
            return Ok(Value::Undefined);
        }
        fields.ordinal_month = Some(month as u8);
    }
    if let Some(code) = vm.get_property(bag, "monthCode")?.as_string() {
        let Ok(month) = Month::try_from_str(&code.to_string_lossy()) else {
            return Ok(Value::Undefined);
        };
        fields.month = Some(month);
    }
    let cal = calendar(args)?;
    let explicit_year = fields.extended_year.is_some();
    let Ok(mut date) = Date::try_from_fields(fields, options, icu_calendar::Ref(&cal)) else {
        return Ok(Value::Undefined);
    };
    if mode == 2 && explicit_year {
        // A MonthDay always stores the canonical ISO reference year, even
        // when an input year determined the ordinal and length of the month.
        let mut reference = DateFields::default();
        reference.month = Some(date.month().to_input());
        reference.day = Some(date.day_of_month().0);
        let Ok(converted) = Date::try_from_fields(reference, options, icu_calendar::Ref(&cal))
        else {
            return Ok(Value::Undefined);
        };
        date = converted;
    }
    let result = iso_record(vm, date.to_rata_die().to_i64_date())?;
    vm.define_data_property(
        result.clone(),
        "calendar",
        args[3].clone(),
        true,
        true,
        true,
    )?;
    Ok(result)
}

/// A fixed-offset zone and a plain civil date/time share the same day and
/// nanosecond balancing. Split the endpoint at the start clock time before
/// asking the calendar for whole years, months, weeks, and days.
pub(super) fn uniform_difference(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use icu_calendar::options::{DateDifferenceOptions, DateDurationUnit};
    use num_traits::ToPrimitive;
    const DAY: i128 = 86_400_000_000_000;
    let epoch = |value: &Value| {
        value
            .as_bigint()
            .filter(|n| n.bit_length() < 95)
            .and_then(|n| n.to_owned_integer().to_i128())
            .ok_or_else(invalid_date)
    };
    let offset = args[3].as_number().ok_or_else(invalid_date)? as i128;
    let from = epoch(&args[1])? + offset;
    let to = epoch(&args[2])? + offset;
    let max_unit = args[4].as_number().ok_or_else(invalid_date)? as usize;
    let invert = if args[5].is_truthy() { -1.0 } else { 1.0 };
    let mut result = [0.0; 10];
    let mut delta = to - from;
    if max_unit >= 7 {
        let a = from.div_euclid(DAY) as i64;
        let mut b = to.div_euclid(DAY) as i64;
        let mut subday = to.rem_euclid(DAY) - from.rem_euclid(DAY);
        if subday.signum() == -delta.signum() {
            b -= delta.signum() as i64;
            subday += delta.signum() * DAY;
        }
        if max_unit == 7 {
            result[2] = ((b - a) / 7) as f64;
            result[3] = ((b - a) % 7) as f64;
        } else {
            if a.abs() > 2_000_000 || b.abs() > 2_000_000 || (a - b).abs() > 73_050 {
                return Ok(Value::Undefined);
            }
            vm.charge_work(1 + (a - b).unsigned_abs() as usize / 365)?;
            let cal = calendar(args)?;
            let a = Date::from_rata_die(RataDie::new(a + UNIX_EPOCH), icu_calendar::Ref(&cal));
            let b = Date::from_rata_die(RataDie::new(b + UNIX_EPOCH), icu_calendar::Ref(&cal));
            let mut options = DateDifferenceOptions::default();
            options.largest_unit = Some(if max_unit == 9 {
                DateDurationUnit::Years
            } else {
                DateDurationUnit::Months
            });
            let d = a
                .try_until_with_options(&b, options)
                .map_err(|_| invalid_date())?;
            let sign = if d.is_negative { -1.0 } else { 1.0 };
            for (v, n) in result[..4]
                .iter_mut()
                .zip([d.years, d.months, d.weeks, d.days])
            {
                *v = f64::from(n) * sign;
            }
        }
        delta = subday;
    }
    let first = if max_unit > 6 { 4 } else { 9 - max_unit };
    for (idx, divisor) in super::duration::NANOS
        .into_iter()
        .enumerate()
        .skip(first - 3)
    {
        result[idx + 3] = (delta / i128::from(divisor)) as f64;
        delta %= i128::from(divisor);
    }
    for v in &mut result {
        *v = if *v == 0.0 { 0.0 } else { *v * invert };
    }
    super::duration::record(vm, result)
}

// A getter only receives a branded instance's internal civil fields. Keep
// dates outside ICU's range on the arbitrary-range calendar implementation.
pub(super) fn derived_field(
    vm: &mut Vm,
    civil: &Value,
    field: &str,
) -> Result<Option<Value>, JsError> {
    let id = match vm.get_property(civil.clone(), "calendar")? {
        Value::Undefined => "iso8601".into(),
        Value::Number(_) => "gregory".into(),
        cal => vm.get_property(cal, "id")?,
    };
    let Some(name) = id.as_string().map(|s| s.to_string_lossy()) else {
        return Ok(None);
    };
    // Historic Persian and Umm al-Qura calculations have prescribed
    // arithmetic fallbacks outside their observational ranges.
    let y = vm
        .get_property(civil.clone(), "year")?
        .as_number()
        .ok_or_else(invalid_date)? as i32;
    if y.abs() > 7000
        || (name == "persian" && !(1900..=2100).contains(&y))
        || (name == "islamic-umalqura" && !(1883..=2076).contains(&y))
    {
        return Ok(None);
    }
    let m = vm
        .get_property(civil.clone(), "month")?
        .as_number()
        .ok_or_else(invalid_date)? as u8;
    let d = vm
        .get_property(civil.clone(), "day")?
        .as_number()
        .ok_or_else(invalid_date)? as u8;
    let date = Date::try_new_iso(y, m, d)
        .map_err(|_| invalid_date())?
        .to_calendar(calendar(&[id])?);
    Ok(Some(match field {
        "inLeapYear" => date.is_in_leap_year().into(),
        "daysInMonth" => u32::from(date.days_in_month()).into(),
        "monthsInYear" => u32::from(date.months_in_year()).into(),
        "daysInYear" => u32::from(date.days_in_year()).into(),
        "dayOfYear" => u32::from(date.day_of_year().0).into(),
        "monthCode" => date.month().to_input().code().0.as_str().into(),
        "day" => u32::from(date.day_of_month().0).into(),
        "month" => u32::from(date.month().ordinal).into(),
        "year" => {
            let year = date.year().extended_year()
                + match name.as_str() {
                    "buddhist" => 543,
                    "roc" => -1911,
                    "ethioaa" => 5500,
                    _ => 0,
                };
            year.into()
        }
        _ => return Ok(None),
    }))
}

pub(super) fn uniform_add(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value, JsError> {
    use num_traits::ToPrimitive;
    const DAY: i128 = 86_400_000_000_000;
    const MAX: i128 = 100_000_000 * DAY;
    let origin = args[1]
        .as_bigint()
        .filter(|n| n.bit_length() < 96)
        .and_then(|n| n.to_owned_integer().to_i128())
        .ok_or_else(invalid_date)?;
    let offset = args[5].as_number().unwrap_or_default() as i128;
    let local = origin + offset;
    let mode = args[4].as_number().ok_or_else(invalid_date)? as u8;
    let bag = args[2].clone();
    let mut field = |key: &str| {
        vm.get_property(bag.clone(), key)?
            .as_number()
            .ok_or_else(invalid_date)
    };
    let years = field("years")?;
    let months = field("months")?;
    let weeks = field("weeks")? as i128;
    let days = field("days")? as i128;
    let mut time = 0i128;
    for (key, scale) in [
        "hours",
        "minutes",
        "seconds",
        "milliseconds",
        "microseconds",
        "nanoseconds",
    ]
    .into_iter()
    .zip(&super::duration::NANOS[1..])
    {
        let n = field(key)?;
        // Interrupted/out-of-range intermediate durations retain the complete
        // JavaScript arithmetic path. Public durations fit comfortably here.
        if n.abs() > 1e25 {
            return Ok(Value::Undefined);
        }
        time += (n as i128) * i128::from(*scale);
    }
    let start = local.div_euclid(DAY) as i64;
    let day = if years == 0.0 && months == 0.0 {
        start
    } else if let Some(v) = add_years_months(
        vm,
        args,
        start as f64,
        years,
        months,
        args[3].as_number() == Some(1.0),
    )? {
        v
    } else {
        return Ok(Value::Undefined);
    };
    if days.abs() > 1_000_000_000 || weeks.abs() > 1_000_000_000 {
        return Err(invalid_date());
    }
    let day = i128::from(day) + weeks * 7 + days;
    let result = if mode == 0 {
        (day + time / DAY) * DAY
    } else {
        day * DAY + local.rem_euclid(DAY) + time - offset
    };
    if result > MAX + if mode == 1 { DAY - 1 } else { 0 }
        || result
            < -MAX
                - if mode == 0 {
                    DAY
                } else if mode == 1 {
                    DAY - 1
                } else {
                    0
                }
    {
        return Err(invalid_date());
    }
    if mode == 0 {
        return super::arithmetic::iso_date(
            vm,
            Value::Undefined,
            &[Value::Number((result / DAY) as f64)],
        );
    }
    let epoch = vm.account_value(Value::BigInt(crate::JsBigInt::from_integer(result.into())?))?;
    if mode == 2 {
        Ok(epoch)
    } else {
        super::arithmetic::epoch_to_civil(vm, Value::Undefined, &[epoch])
    }
}

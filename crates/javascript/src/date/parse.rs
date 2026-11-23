use super::{
    calendar::{
        DAY_MS, HOUR_MS, MINUTE_MS, MONTHS, SECOND_MS, WEEKDAYS, days_from_civil, time_clip,
    },
    local,
};
use crate::{JsError, JsString, Vm};

const MAX_DATE_UNITS: usize = 512;

#[derive(Clone, Copy)]
struct Parsed {
    year: i64,
    month: u32,
    day: u32,
    clock: i64,
    /// East of UTC in milliseconds. None means the host local timezone.
    offset: Option<i64>,
}

impl Parsed {
    fn new(year: i64, month: u32, day: u32, clock: i64, offset: Option<i64>) -> Option<Self> {
        if year.abs() > 999_999 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(Self {
            year,
            month,
            day,
            clock,
            offset,
        })
    }

    fn instant(self) -> f64 {
        let wall = days_from_civil(self.year, self.month, self.day) * DAY_MS + self.clock;
        let utc = self.offset.map_or_else(
            || local::to_utc(wall as f64),
            |offset| (wall - offset) as f64,
        );
        time_clip(utc)
    }
}

pub(super) fn parse(vm: &mut Vm, text: &JsString) -> Result<f64, JsError> {
    let units = text.as_utf16();
    if units.len() > MAX_DATE_UNITS {
        return Ok(f64::NAN);
    }
    vm.consume_work(units.len())?;
    let mut ascii = [0_u8; MAX_DATE_UNITS];
    let Some(length) = ascii_date(units, &mut ascii) else {
        return Ok(f64::NAN);
    };
    let ascii = std::str::from_utf8(&ascii[..length]).unwrap().trim_ascii();
    // A syntactically valid ISO string always owns the result, including NaN
    // when the parsed instant exceeds the supported time-value range.
    Ok(iso(ascii.as_bytes())
        .or_else(|| legacy(ascii))
        .map_or(f64::NAN, Parsed::instant))
}

fn ascii_date(units: &[u16], ascii: &mut [u8]) -> Option<usize> {
    let mut depth = 0;
    let mut len = 0;
    for &unit in units {
        match unit {
            40 => {
                if depth == 0 {
                    ascii[len] = b' ';
                    len += 1;
                }
                depth += 1;
            }
            41 => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            _ if depth > 0 => {}
            0..=127 => {
                ascii[len] = unit as u8;
                len += 1;
            }
            _ => return None,
        }
    }
    (depth == 0).then_some(len)
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, byte: u8) -> bool {
        if self.0.first() == Some(&byte) {
            self.0 = &self.0[1..];
            true
        } else {
            false
        }
    }

    fn digits(&mut self, count: usize) -> Option<u32> {
        let digits = self.0.get(..count)?;
        let mut value = 0;
        for digit in digits {
            if !digit.is_ascii_digit() {
                return None;
            }
            value = value * 10 + u32::from(digit - b'0');
        }
        self.0 = &self.0[count..];
        Some(value)
    }

    fn sign(&mut self) -> Option<i64> {
        if self.take(b'+') {
            Some(1)
        } else if self.take(b'-') {
            Some(-1)
        } else {
            None
        }
    }

    fn clock(&mut self) -> Option<i64> {
        let hour = self.digits(2)?;
        if !self.take(b':') {
            return None;
        }
        let minute = self.digits(2)?;
        let mut second = 0;
        let mut millisecond = 0;
        if self.take(b':') {
            second = self.digits(2)?;
            if self.take(b'.') {
                let fraction = self
                    .0
                    .iter()
                    .take_while(|digit| digit.is_ascii_digit())
                    .count();
                if fraction == 0 {
                    return None;
                }
                millisecond =
                    self.digits(fraction.min(3))? * 10_u32.pow(3 - fraction.min(3) as u32);
                self.0 = &self.0[fraction.saturating_sub(3)..];
            }
        }
        if minute > 59
            || second > 59
            || hour > 24
            || (hour == 24 && (minute != 0 || second != 0 || millisecond != 0))
        {
            return None;
        }
        Some(
            i64::from(hour) * HOUR_MS
                + i64::from(minute) * MINUTE_MS
                + i64::from(second) * SECOND_MS
                + i64::from(millisecond),
        )
    }

    fn offset(&mut self, sign: i64) -> Option<i64> {
        let hours = self.digits(2)?;
        self.take(b':');
        let minutes = self.digits(2)?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        Some(sign * (i64::from(hours) * HOUR_MS + i64::from(minutes) * MINUTE_MS))
    }
}

fn iso(input: &[u8]) -> Option<Parsed> {
    let mut input = Reader(input);
    let year = if let Some(sign) = input.sign() {
        let digits = input.digits(6)?;
        if sign < 0 && digits == 0 {
            return None;
        }
        sign * i64::from(digits)
    } else {
        i64::from(input.digits(4)?)
    };
    let mut month = 1;
    let mut day = 1;
    if input.take(b'-') {
        month = input.digits(2)?;
        if input.take(b'-') {
            day = input.digits(2)?;
        }
    }
    if input.0.is_empty() {
        return Parsed::new(year, month, day, 0, Some(0));
    }
    // Lower-case 't' and common space-separated SQL/RFC date-times share the
    // same field validation as the ISO form.
    if !input.take(b'T') && !input.take(b't') && !input.take(b' ') {
        return None;
    }
    let clock = input.clock()?;
    let offset = if input.take(b'Z') || input.take(b'z') {
        Some(0)
    } else if let Some(sign) = input.sign() {
        Some(input.offset(sign)?)
    } else {
        None
    };
    if !input.0.is_empty() {
        return None;
    }
    Parsed::new(year, month, day, clock, offset)
}

fn month_name(text: &str) -> Option<u32> {
    const LONG_MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    MONTHS
        .iter()
        .zip(LONG_MONTHS)
        .position(|(short, long)| {
            text.eq_ignore_ascii_case(short) || text.eq_ignore_ascii_case(long)
        })
        .map(|index| index as u32 + 1)
}

fn legacy_year(token: &str) -> Option<i64> {
    let year = token.parse::<i32>().ok()?;
    if token.len() <= 2 && year >= 0 {
        Some(i64::from(year + if year < 50 { 2000 } else { 1900 }))
    } else {
        Some(i64::from(year))
    }
}

fn numeric_date(token: &str) -> Option<(i64, u32, u32)> {
    let delimiter = if token.contains('/') { '/' } else { '-' };
    let mut parts = token.split(delimiter);
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if first.len() >= 4 {
        Some((
            legacy_year(first)?,
            second.parse().ok()?,
            third.parse().ok()?,
        ))
    } else {
        Some((
            legacy_year(third)?,
            first.parse().ok()?,
            second.parse().ok()?,
        ))
    }
}

fn legacy_offset(token: &str) -> Option<i64> {
    let abbreviations = [
        ("GMT", 0),
        ("UTC", 0),
        ("UT", 0),
        ("Z", 0),
        ("EST", -5),
        ("EDT", -4),
        ("CST", -6),
        ("CDT", -5),
        ("MST", -7),
        ("MDT", -6),
        ("PST", -8),
        ("PDT", -7),
    ];
    if let Some((_, offset)) = abbreviations
        .iter()
        .find(|(name, _)| token.eq_ignore_ascii_case(name))
    {
        return Some(i64::from(*offset) * HOUR_MS);
    }
    let body = match token.get(..3) {
        Some(prefix)
            if prefix.eq_ignore_ascii_case("GMT") || prefix.eq_ignore_ascii_case("UTC") =>
        {
            &token[3..]
        }
        _ => token,
    };
    let mut reader = Reader(body.as_bytes());
    let sign = reader.sign()?;
    let offset = reader.offset(sign)?;
    reader.0.is_empty().then_some(offset)
}

fn legacy(input: &str) -> Option<Parsed> {
    let mut parts = input
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|part| !part.is_empty())
        .peekable();
    if parts
        .peek()
        .is_some_and(|part| WEEKDAYS.iter().any(|day| part.eq_ignore_ascii_case(day)))
    {
        parts.next();
    }
    let first = parts.next()?;
    let (year, month, day) = if let Some(month) = month_name(first) {
        let day = parts.next()?.parse().ok()?;
        (legacy_year(parts.next()?)?, month, day)
    } else if first.contains('/') || first.contains('-') {
        numeric_date(first)?
    } else {
        let day = first.parse().ok()?;
        let month = month_name(parts.next()?)?;
        (legacy_year(parts.next()?)?, month, day)
    };
    let clock = if parts
        .peek()
        .is_some_and(|part| part.contains(':') && !part.starts_with(['+', '-']))
    {
        let mut clock = Reader(parts.next()?.as_bytes());
        let value = clock.clock()?;
        if !clock.0.is_empty() {
            return None;
        }
        value
    } else {
        0
    };
    let offset = if let Some(part) = parts.next() {
        Some(legacy_offset(part)?)
    } else {
        None
    };
    if parts.next().is_some() {
        return None;
    }
    Parsed::new(year, month, day, clock, offset)
}

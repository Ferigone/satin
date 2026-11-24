//! Outside ICU4X Date's civil range, extend its mean solar-term and new-moon
//! model for Chinese and Korean calendars. The month that contains winter
//! solstice is M11. A month lacking a major solar term repeats its predecessor.
//! Epochs and mean periods follow ICU4X's East Asian traditional calendar.
const DAY: i64 = 86_400_000;
const YEAR: i64 = DAY * 146_097 / 400;
const MONTH: i64 = 2_551_442_876;
const NEW_MOON: i64 = 947_182_440_000; // 2000-01-06 18:14 UTC
const SOLSTICE: i64 = 945_848_640_000; // 1999-12-22 07:44 UTC

pub(super) fn months(calendar: &str, year: i32) -> Vec<(i64, u8)> {
    let offset = match calendar {
        "dangi" => 9 * 3_600_000,
        "chinese" if year < 1900 => 27_940_000, // historical Beijing local mean time
        _ => 8 * 3_600_000,
    };
    let mut term = SOLSTICE + i64::from(year - 2000) * YEAR + offset;
    let last_ms = (term.div_euclid(DAY) + 1) * DAY - 1;
    let base = NEW_MOON + offset;
    let mut moon = base + (last_ms - base).div_euclid(MONTH) * MONTH;
    let mut number = -1i32; // M11 and M12 of the preceding lunar year
    let mut repeated = false;
    let mut result = Vec::with_capacity(13);
    loop {
        let next = moon + MONTH;
        let leap = !repeated && next.div_euclid(DAY) <= term.div_euclid(DAY);
        let code = if leap { number - 1 } else { number };
        if code > 12 {
            break;
        }
        if code > 0 {
            result.push((moon.div_euclid(DAY), code as u8));
        }
        if leap {
            repeated = true;
        } else {
            number += 1;
            term += YEAR / 12;
        }
        moon = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use icu_calendar::{AnyCalendar, AnyCalendarKind, Date};

    #[test]
    fn extended_months_join_icu_and_stay_contiguous_across_years() {
        for (id, kind) in [
            ("chinese", AnyCalendarKind::Chinese),
            ("dangi", AnyCalendarKind::Dangi),
        ] {
            for year in [-9001, -9000, 8999, 9000] {
                let cal = AnyCalendar::new(kind);
                let starts = months(id, year);
                for (ordinal, (epoch, number)) in starts.iter().enumerate() {
                    let d = Date::from_rata_die(
                        icu_calendar::types::RataDie::new(epoch + 719163),
                        icu_calendar::Ref(&cal),
                    );
                    assert_eq!(
                        (
                            d.year().extended_year(),
                            d.month().ordinal,
                            d.month().number(),
                            d.day_of_month().0
                        ),
                        (year, (ordinal + 1) as u8, *number, 1)
                    );
                }
            }
            for year in [-271821, -250000, 250000, 275760] {
                let starts = months(id, year);
                let end = months(id, year + 1)[0].0;
                assert!((12..=13).contains(&starts.len()));
                for (i, (start, _)) in starts.iter().enumerate() {
                    let next = starts.get(i + 1).map_or(end, |m| m.0);
                    assert!((29..=30).contains(&(next - start)));
                }
            }
        }
    }
}

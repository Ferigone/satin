//! Proleptic Gregorian dates. A civil date has no timezone.

pub(super) const SECOND_MS: i64 = 1_000;
pub(super) const MINUTE_MS: i64 = 60 * SECOND_MS;
pub(super) const HOUR_MS: i64 = 60 * MINUTE_MS;
pub(super) const DAY_MS: i64 = 24 * HOUR_MS;
pub(super) const CLIP_LIMIT: f64 = 8_640_000_000_000_000.0;

pub(super) const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub(super) const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Clone, Copy)]
pub(super) struct CivilTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

impl CivilTime {
    pub fn from_epoch(time: i64) -> Self {
        let days = time.div_euclid(DAY_MS);
        let within_day = time.rem_euclid(DAY_MS);

        // Count eras from March of year 0, so each leap day ends a year.
        let march_days = days + 719_468;
        let era = march_days.div_euclid(146_097);
        let day_of_era = march_days.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let march_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * march_month + 2) / 5 + 1;
        let month = if march_month < 10 {
            march_month + 3
        } else {
            march_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);

        Self {
            year,
            month: month as u32,
            day: day as u32,
            weekday: (days + 4).rem_euclid(7) as u32,
            hour: (within_day / HOUR_MS) as u32,
            minute: (within_day / MINUTE_MS % 60) as u32,
            second: (within_day / SECOND_MS % 60) as u32,
            millisecond: (within_day % SECOND_MS) as u32,
        }
    }

    pub fn numbers(self) -> [f64; 7] {
        [
            self.year as f64,
            (self.month - 1) as f64,
            self.day as f64,
            self.hour as f64,
            self.minute as f64,
            self.second as f64,
            self.millisecond as f64,
        ]
    }

    #[cfg(any(windows, test))]
    pub fn in_year(self, year: i64) -> i64 {
        days_from_civil(year, self.month, self.day) * DAY_MS
            + self.hour as i64 * HOUR_MS
            + self.minute as i64 * MINUTE_MS
            + self.second as i64 * SECOND_MS
            + self.millisecond as i64
    }
}

#[cfg(windows)]
pub(super) fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub(super) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = i64::from((153 * month_from_march + 2) / 5 + day - 1);
    let day_of_era = 365 * year_of_era + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub(super) fn make_full_year(year: f64) -> f64 {
    let year = year.trunc();
    if (0.0..=99.0).contains(&year) {
        1900.0 + year
    } else {
        year
    }
}

pub(super) fn make_date(fields: [f64; 7]) -> f64 {
    let [year, month, day, hour, minute, second, millisecond] = fields;
    if !fields.into_iter().all(f64::is_finite) {
        return f64::NAN;
    }
    let month = month.trunc();
    let balanced_year = year.trunc() + (month / 12.0).floor();
    // A civil year beyond this range has no millisecond-based Number time
    // value from which MakeDay can recover its month start.
    if !balanced_year.is_finite() || balanced_year.abs() > (i64::MAX / 366) as f64 {
        return f64::NAN;
    }
    let balanced_month = month.rem_euclid(12.0) as u32 + 1;
    let ordinal = days_from_civil(balanced_year as i64, balanced_month, 1) as f64;
    let days = ordinal + day.trunc() - 1.0;
    // As in the ECMAScript MakeTime algorithm, this is ordered f64 arithmetic.
    let time = ((hour.trunc() * HOUR_MS as f64 + minute.trunc() * MINUTE_MS as f64)
        + second.trunc() * SECOND_MS as f64)
        + millisecond.trunc();
    days * DAY_MS as f64 + time
}

pub(super) fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > CLIP_LIMIT {
        f64::NAN
    } else {
        // Discard sub-millisecond precision and canonicalize negative zero.
        time.trunc() + 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gregorian_conversion_at_epoch_and_clip_boundaries() {
        for (time, expected) in [
            (-8_640_000_000_000_000, (-271821, 4, 20)),
            (-62_167_219_200_000, (0, 1, 1)),
            (-1, (1969, 12, 31)),
            (0, (1970, 1, 1)),
            (951_782_400_000, (2000, 2, 29)),
            (8_640_000_000_000_000, (275760, 9, 13)),
        ] {
            let date = CivilTime::from_epoch(time);
            assert_eq!((date.year, date.month, date.day), expected);
            assert_eq!(date.in_year(date.year), time);
        }
        assert_eq!(time_clip(-0.9).to_bits(), 0);
    }
}

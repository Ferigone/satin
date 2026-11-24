//! Host timezone adapter. Local wall times are resolved with ECMAScript's
//! compatible disambiguation at offset transitions.
use super::calendar::{CLIP_LIMIT, DAY_MS};

pub(super) use platform::Zone;

pub(super) fn to_utc(wall: f64) -> f64 {
    if !wall.is_finite() || wall.abs() > CLIP_LIMIT + DAY_MS as f64 {
        return f64::NAN;
    }
    Zone::system().utc_from_wall(wall as i64) as f64
}

fn resolve_wall(
    wall: i64,
    offsets: impl IntoIterator<Item = i64>,
    offset_at: impl Fn(i64) -> i64,
) -> i64 {
    offsets
        .into_iter()
        .filter_map(|offset| {
            let utc = wall - offset;
            (offset_at(utc) == offset).then_some(utc)
        })
        // For a repeated wall time, choose its first occurrence.
        .min()
        .unwrap_or_else(|| {
            // In a gap use the actual offset before the transition. Guessed
            // biases from other rule years must not determine the adjustment.
            wall - offset_at(wall - DAY_MS)
        })
}

#[cfg(windows)]
mod platform {
    use super::resolve_wall;
    use crate::date::calendar::{CivilTime, DAY_MS, MINUTE_MS, days_from_civil, is_leap_year};
    use std::mem::MaybeUninit;
    use windows_sys::Win32::{
        Foundation::SYSTEMTIME,
        System::Time::{
            DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation,
            GetTimeZoneInformationForYear, SystemTimeToTzSpecificLocalTimeEx, TIME_ZONE_ID_INVALID,
            TIME_ZONE_INFORMATION,
        },
    };

    pub(in crate::date) struct Zone(Option<DYNAMIC_TIME_ZONE_INFORMATION>);

    impl Zone {
        pub fn system() -> Self {
            let mut info = MaybeUninit::uninit();
            // Win32 writes the complete POD structure on success.
            let code = unsafe { GetDynamicTimeZoneInformation(info.as_mut_ptr()) };
            let info = (code != TIME_ZONE_ID_INVALID).then(|| unsafe { info.assume_init() });
            Self(info)
        }

        fn for_year(&self, year: i64) -> Option<TIME_ZONE_INFORMATION> {
            let zone = self.0.as_ref()?;
            let mut info = MaybeUninit::uninit();
            // Both pointers are valid for their declared C structures.
            let result =
                unsafe { GetTimeZoneInformationForYear(year as u16, zone, info.as_mut_ptr()) };
            (result != 0).then(|| unsafe { info.assume_init() })
        }

        pub fn offset_at(&self, utc: i64) -> i64 {
            let Some(zone) = &self.0 else { return 0 };
            let date = CivilTime::from_epoch(utc);
            let year = supported_year(date.year);
            let input = SYSTEMTIME {
                wYear: year as u16,
                wMonth: date.month as u16,
                wDayOfWeek: date.weekday as u16,
                wDay: date.day as u16,
                wHour: date.hour as u16,
                wMinute: date.minute as u16,
                wSecond: date.second as u16,
                wMilliseconds: date.millisecond as u16,
            };
            let mut output = MaybeUninit::uninit();
            // The input is a bounded valid calendar time. The output is
            // initialized by the API on success.
            let result =
                unsafe { SystemTimeToTzSpecificLocalTimeEx(zone, &input, output.as_mut_ptr()) };
            if result != 0 {
                let output = unsafe { output.assume_init() };
                let wall = days_from_civil(
                    i64::from(output.wYear),
                    u32::from(output.wMonth),
                    u32::from(output.wDay),
                ) * DAY_MS
                    + i64::from(output.wHour) * 3_600_000
                    + i64::from(output.wMinute) * MINUTE_MS
                    + i64::from(output.wSecond) * 1_000
                    + i64::from(output.wMilliseconds);
                return wall - date.in_year(year);
            }
            to_offset(zone.Bias, zone.StandardBias)
        }

        pub fn utc_from_wall(&self, wall: i64) -> i64 {
            let Some(zone) = &self.0 else { return wall };
            let date = CivilTime::from_epoch(wall);
            let year = supported_year(date.year);
            let initial = self.offset_at(wall);
            let mut offsets = [initial; 7];
            offsets[1] = to_offset(zone.Bias, zone.StandardBias);
            offsets[2] = to_offset(zone.Bias, zone.DaylightBias);
            if let Some(rule) = self.for_year(year) {
                offsets[3] = to_offset(rule.Bias, rule.StandardBias);
                offsets[4] = to_offset(rule.Bias, rule.DaylightBias);
            }
            // A candidate UTC instant near New Year can use the adjacent rule.
            let adjacent = match (date.month, date.day) {
                (1, 1) => Some(supported_year(date.year - 1)),
                (12, 31) => Some(supported_year(date.year + 1)),
                _ => None,
            };
            if let Some(rule) = adjacent.and_then(|year| self.for_year(year)) {
                offsets[5] = to_offset(rule.Bias, rule.StandardBias);
                offsets[6] = to_offset(rule.Bias, rule.DaylightBias);
            }
            offsets.sort_unstable();
            resolve_wall(
                wall,
                offsets
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, offset)| {
                        (index == 0 || offsets[index - 1] != offset).then_some(offset)
                    }),
                |utc| self.offset_at(utc),
            )
        }

        pub fn name_at(&self, utc: i64, offset: i64) -> String {
            let Some(zone) = &self.0 else {
                return "UTC".to_owned();
            };
            let date = CivilTime::from_epoch(utc + offset);
            let (standard, daylight) = self.for_year(supported_year(date.year)).map_or(
                (
                    to_offset(zone.Bias, zone.StandardBias),
                    to_offset(zone.Bias, zone.DaylightBias),
                ),
                |rule| {
                    (
                        to_offset(rule.Bias, rule.StandardBias),
                        to_offset(rule.Bias, rule.DaylightBias),
                    )
                },
            );
            let name = if offset == daylight && daylight != standard {
                &zone.DaylightName
            } else {
                &zone.StandardName
            };
            let length = name
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(name.len());
            let name = String::from_utf16_lossy(&name[..length]);
            if name.is_empty() {
                "UTC".to_owned()
            } else {
                name
            }
        }
    }

    fn to_offset(bias: i32, seasonal_bias: i32) -> i64 {
        -(i64::from(bias) + i64::from(seasonal_bias)) * MINUTE_MS
    }

    fn supported_year(year: i64) -> i64 {
        // Keep a one-day margin inside Win32's SYSTEMTIME conversion range.
        if (1602..=30_826).contains(&year) {
            return year;
        }
        let weekday = days_from_civil(year, 1, 1).rem_euclid(7);
        (2008..2036)
            .find(|candidate| {
                is_leap_year(*candidate) == is_leap_year(year)
                    && days_from_civil(*candidate, 1, 1).rem_euclid(7) == weekday
            })
            .expect("an equivalent Gregorian year in 28 years without a century boundary")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Time::EnumDynamicTimeZoneInformation;

        fn eastern() -> Zone {
            let key: Vec<_> = "Eastern Standard Time".encode_utf16().collect();
            for index in 0..512 {
                let mut info = MaybeUninit::<DYNAMIC_TIME_ZONE_INFORMATION>::uninit();
                // The enumerator returns ERROR_SUCCESS when it wrote a record.
                let status = unsafe { EnumDynamicTimeZoneInformation(index, info.as_mut_ptr()) };
                if status != 0 {
                    break;
                }
                let info = unsafe { info.assume_init() };
                if info.TimeZoneKeyName.starts_with(&key)
                    && info.TimeZoneKeyName.get(key.len()) == Some(&0)
                {
                    return Zone(Some(info));
                }
            }
            panic!("Windows Eastern Standard Time zone is missing");
        }

        #[test]
        fn win32_dynamic_rules_drive_gaps_overlaps_and_seasonal_offsets() {
            let zone = eastern();
            let hour = 60 * MINUTE_MS;
            let winter = days_from_civil(2024, 1, 25) * DAY_MS;
            let summer = days_from_civil(2024, 7, 25) * DAY_MS;
            assert_eq!(zone.offset_at(winter), -5 * hour);
            assert_eq!(zone.offset_at(summer), -4 * hour);

            let gap = days_from_civil(2024, 3, 10) * DAY_MS + 2 * hour + 30 * MINUTE_MS;
            let overlap = days_from_civil(2024, 11, 3) * DAY_MS + hour + 30 * MINUTE_MS;
            assert_eq!(zone.utc_from_wall(gap), gap + 5 * hour);
            assert_eq!(zone.utc_from_wall(overlap), overlap + 4 * hour);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{DAY_MS, resolve_wall};
    use crate::date::calendar::project_timestamp;
    use jiff::{Timestamp, tz::TimeZone};

    pub(in crate::date) struct Zone(TimeZone);

    fn stamp(ms: i64) -> Timestamp {
        project_timestamp(
            ms.div_euclid(1000),
            (ms.rem_euclid(1000) * 1_000_000) as i32,
        )
        .0
    }

    impl Zone {
        pub fn system() -> Self {
            Self(TimeZone::system())
        }
        pub fn offset_at(&self, ms: i64) -> i64 {
            i64::from(self.0.to_offset(stamp(ms)).seconds()) * 1000
        }
        pub fn utc_from_wall(&self, wall: i64) -> i64 {
            let offsets = [
                self.offset_at(wall - DAY_MS),
                self.offset_at(wall),
                self.offset_at(wall + DAY_MS),
            ];
            resolve_wall(wall, offsets, |ms| self.offset_at(ms))
        }
        pub fn name_at(&self, ms: i64, _: i64) -> String {
            self.0.to_offset_info(stamp(ms)).abbreviation().to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{super::calendar::MINUTE_MS, resolve_wall};

    #[test]
    fn repeated_wall_time_chooses_the_earlier_instant() {
        let offset_at = |utc| {
            if utc < 0 {
                120 * MINUTE_MS
            } else {
                60 * MINUTE_MS
            }
        };
        let wall = 90 * MINUTE_MS;
        assert_eq!(
            resolve_wall(wall, [60 * MINUTE_MS, 120 * MINUTE_MS], offset_at),
            -30 * MINUTE_MS
        );
    }

    #[test]
    fn missing_wall_time_uses_offset_before_increase() {
        let offset_at = |utc| {
            if utc < 0 {
                60 * MINUTE_MS
            } else {
                90 * MINUTE_MS
            }
        };
        let wall = 75 * MINUTE_MS;
        assert_eq!(
            resolve_wall(wall, [60 * MINUTE_MS, 90 * MINUTE_MS], offset_at),
            15 * MINUTE_MS
        );
    }

    #[test]
    fn gap_ignores_offsets_from_other_rule_years() {
        let before = 30 * MINUTE_MS;
        let after = 90 * MINUTE_MS;
        let offset_at = |utc| if utc < 0 { before } else { after };
        let wall = 75 * MINUTE_MS;
        let other_rule = 60 * MINUTE_MS;
        assert_eq!(
            resolve_wall(wall, [before, after, other_rule], offset_at),
            wall - before
        );
    }
}

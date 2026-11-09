//! Dates for the Snapshots calendar, in the machine's local time.
//!
//! Set ids and update records are unix seconds. The calendar files each one
//! under the local day it was made on, so an evening update is not drawn on
//! tomorrow's square. The offset comes from `localtime_r(3)`, which reads
//! `$TZ` or `/etc/localtime` and knows about daylight saving.

use crate::immutable::history::{civil_from_days, days_from_civil};

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTH_NAMES: [&str; 12] = [
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

/// Seconds east of UTC at `secs`, or 0 when the C library cannot say.
pub fn utc_offset(secs: u64) -> i64 {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain old data, and localtime_r writes only to it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if converted {
        tm.tm_gmtoff
    } else {
        0
    }
}

/// Local wall-clock seconds, counted as if the local zone were UTC.
fn local_secs(secs: u64) -> i64 {
    secs as i64 + utc_offset(secs)
}

/// The local day `secs` falls on, counted from 1970-01-01.
pub fn local_day(secs: u64) -> i64 {
    local_secs(secs).div_euclid(86_400)
}

/// `HH:MM`, local.
pub fn local_time(secs: u64) -> String {
    let rem = local_secs(secs).rem_euclid(86_400);
    format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}

/// `YYYY-MM-DD HH:MM`, local.
pub fn local_timestamp(secs: u64) -> String {
    let (y, m, d) = civil_from_days(local_day(secs));
    format!("{y:04}-{m:02}-{d:02} {}", local_time(secs))
}

/// The day of the week, 0 for Monday to 6 for Sunday. 1970-01-01 was a
/// Thursday.
pub fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

/// The day of the month, 1 to 31.
pub fn day_of_month(day: i64) -> u32 {
    civil_from_days(day).2
}

/// "Sunday 8 November 2026".
pub fn long_date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!(
        "{} {d} {} {y}",
        WEEKDAY_NAMES[weekday(day)],
        MONTH_NAMES[m as usize - 1]
    )
}

/// One calendar month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
}

impl Month {
    /// The month `day` falls in.
    pub fn of_day(day: i64) -> Self {
        let (year, month, _) = civil_from_days(day);
        Self { year, month }
    }

    pub fn first_day(self) -> i64 {
        days_from_civil(self.year, self.month, 1)
    }

    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                month: self.month + 1,
                ..self
            }
        }
    }

    pub fn prev(self) -> Self {
        if self.month == 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                month: self.month - 1,
                ..self
            }
        }
    }

    /// How many days the month has.
    pub fn days(self) -> u32 {
        (self.next().first_day() - self.first_day()) as u32
    }

    pub fn contains(self, day: i64) -> bool {
        Self::of_day(day) == self
    }

    /// The six Monday-first weeks that hold the month, as days. Always six, so
    /// the grid does not change height from one month to the next.
    pub fn grid(self) -> [i64; 42] {
        let first = self.first_day();
        let start = first - weekday(first) as i64;
        std::array::from_fn(|i| start + i as i64)
    }

    /// "November 2026".
    pub fn label(self) -> String {
        format!("{} {}", MONTH_NAMES[self.month as usize - 1], self.year)
    }
}

/// What the Snapshots tab has picked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Day(i64),
    /// The restore points without a date: the base install, and a slot no
    /// update has been recorded against.
    Undated,
}

/// The Snapshots tab's view: which month is shown and what is selected.
/// `None` until the user moves it, which shows the newest snapshot's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    pub month: Option<Month>,
    pub selected: Option<Selection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOV_8_2026: i64 = 20_765;

    #[test]
    fn weekdays_start_on_monday() {
        assert_eq!(WEEKDAYS[weekday(0)], "Thu", "1970-01-01");
        assert_eq!(WEEKDAYS[weekday(NOV_8_2026)], "Sun");
        assert_eq!(long_date(NOV_8_2026), "Sunday 8 November 2026");
    }

    #[test]
    fn the_grid_is_six_monday_first_weeks_around_the_month() {
        let nov = Month::of_day(NOV_8_2026);
        assert_eq!(nov.label(), "November 2026");
        let grid = nov.grid();
        assert_eq!(weekday(grid[0]), 0);
        // 1 November 2026 is a Sunday, so the grid opens on Monday 26 October.
        assert_eq!(day_of_month(grid[0]), 26);
        assert_eq!(grid[6], nov.first_day());
        assert!(grid.iter().filter(|d| nov.contains(**d)).count() == 30);
    }

    #[test]
    fn months_step_across_year_ends_and_know_their_length() {
        let dec = Month {
            year: 2026,
            month: 12,
        };
        assert_eq!(
            dec.next(),
            Month {
                year: 2027,
                month: 1
            }
        );
        assert_eq!(dec.next().prev(), dec);
        assert_eq!(
            Month {
                year: 2024,
                month: 2
            }
            .days(),
            29
        );
        assert_eq!(
            Month {
                year: 2026,
                month: 2
            }
            .days(),
            28
        );
        assert_eq!(dec.days(), 31);
    }

    #[test]
    fn the_local_offset_is_a_real_time_zone() {
        let offset = utc_offset(1_794_188_708);
        assert!(offset.abs() <= 14 * 3600, "{offset}");
        assert_eq!(offset % 60, 0);
    }

    #[test]
    fn local_time_and_day_agree() {
        let secs = 1_794_188_708;
        let stamp = local_timestamp(secs);
        assert!(stamp.ends_with(&local_time(secs)), "{stamp}");
        let (y, m, d) = civil_from_days(local_day(secs));
        assert!(
            stamp.starts_with(&format!("{y:04}-{m:02}-{d:02}")),
            "{stamp}"
        );
    }
}

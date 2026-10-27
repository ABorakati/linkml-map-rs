//! Python `datetime.date` / `datetime.datetime` values, as PyYAML's
//! `SafeLoader` produces them.
//!
//! An unquoted YAML scalar such as `2024-01-01` or `2024-01-01 10:30:00Z`
//! resolves to the `!!timestamp` tag, which `yaml.safe_load` constructs as a
//! `datetime.date` / `datetime.datetime` (`SafeConstructor.construct_yaml_timestamp`).
//! Upstream then carries those objects through the transform untouched, and
//! what they look like afterwards depends on the writer: PyYAML emits them as
//! plain timestamps (`2024-01-01`, `2024-01-01 10:30:00+00:00`), the TSV/CSV
//! writers use `str()`, and `simplejson` refuses them (`TypeError: Object of type
//! date is not JSON serializable`). [`Value`](crate::value::Value) therefore
//! needs a value that is *not* a string but prints like one: [`Temporal`].
//!
//! Only what upstream can observe is modelled: construction from a YAML
//! timestamp, `str()` (= `isoformat(' ')` for a datetime, `isoformat()` for a
//! date) and equality.

use std::fmt;

/// A calendar date (`datetime.date`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

/// A date and time of day (`datetime.datetime`), naive or with a fixed UTC
/// offset (`datetime.timezone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    date: Date,
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    /// Offset from UTC in minutes; `None` for a naive datetime.
    utc_offset_minutes: Option<i32>,
}

/// A value PyYAML builds from a `!!timestamp` scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temporal {
    /// `datetime.date`.
    Date(Date),
    /// `datetime.datetime`.
    DateTime(DateTime),
}

impl Temporal {
    /// Python's `repr()`: `datetime.date(2024, 1, 1)`,
    /// `datetime.datetime(2024, 1, 1, 10, 0, tzinfo=datetime.timezone.utc)`.
    pub fn repr(&self) -> String {
        match self {
            Temporal::Date(d) => format!("datetime.date({}, {}, {})", d.year, d.month, d.day),
            Temporal::DateTime(dt) => {
                let d = dt.date;
                let mut out = format!(
                    "datetime.datetime({}, {}, {}, {}, {}",
                    d.year, d.month, d.day, dt.hour, dt.minute
                );
                // The seconds and microseconds are dropped while zero.
                if dt.second != 0 || dt.microsecond != 0 {
                    out.push_str(&format!(", {}", dt.second));
                }
                if dt.microsecond != 0 {
                    out.push_str(&format!(", {}", dt.microsecond));
                }
                match dt.utc_offset_minutes {
                    None => {}
                    Some(0) => out.push_str(", tzinfo=datetime.timezone.utc"),
                    Some(minutes) => {
                        // `repr(timedelta)`: `days` floors, `seconds` is the
                        // non-negative remainder.
                        let total = i64::from(minutes) * 60;
                        let (days, seconds) = (total.div_euclid(86_400), total.rem_euclid(86_400));
                        let mut parts = Vec::new();
                        if days != 0 {
                            parts.push(format!("days={days}"));
                        }
                        if seconds != 0 {
                            parts.push(format!("seconds={seconds}"));
                        }
                        out.push_str(&format!(
                            ", tzinfo=datetime.timezone(datetime.timedelta({}))",
                            parts.join(", ")
                        ));
                    }
                }
                out.push(')');
                out
            }
        }
    }

    /// The Python class name (`type(v).__name__`), as it appears in upstream's
    /// error messages: `"date"` or `"datetime"`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Temporal::Date(_) => "date",
            Temporal::DateTime(_) => "datetime",
        }
    }
}

fn is_leap(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Parse a run of ASCII digits (the regular expression already guarantees the
/// shape; this only converts).
fn digits(s: &str) -> u32 {
    s.bytes()
        .fold(0u32, |acc, b| acc * 10 + u32::from(b - b'0'))
}

impl Temporal {
    /// `SafeConstructor.construct_yaml_timestamp`: build the value from the
    /// scalar text, which must match the constructor's `timestamp_regexp`
    /// (the implicit resolver's pattern is a subset of it).
    ///
    /// Out-of-range fields (`2024-13-45`, hour 25, a UTC offset of a day or
    /// more) raise `ValueError` upstream, which `yaml.safe_load` lets through;
    /// they are an `Err` with Python's message here.
    pub fn from_yaml_timestamp(text: &str) -> Result<Self, String> {
        let no_match = || format!("timestamp does not match PyYAML's pattern: {text:?}");
        let (year, rest) = text.split_at_checked(4).ok_or_else(no_match)?;
        if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
            return Err(no_match());
        }
        let rest = rest.strip_prefix('-').ok_or_else(no_match)?;
        let (month, rest) = take_digits(rest, 1, 2).ok_or_else(no_match)?;
        let rest = rest.strip_prefix('-').ok_or_else(no_match)?;
        let (day, rest) = take_digits(rest, 1, 2).ok_or_else(no_match)?;

        let (year, month, day) = (digits(year), digits(month) as u8, digits(day) as u8);
        if year < 1 {
            return Err(format!("year {year} is out of range"));
        }
        if !(1..=12).contains(&month) {
            return Err("month must be in 1..12".to_owned());
        }
        if day < 1 || day > days_in_month(year as u16, month) {
            return Err("day is out of range for month".to_owned());
        }
        let date = Date {
            year: year as u16,
            month,
            day,
        };
        if rest.is_empty() {
            return Ok(Temporal::Date(date));
        }

        // (?:[Tt]|[ \t]+)
        let rest = match rest.strip_prefix(['T', 't']) {
            Some(r) => r,
            None => {
                let r = rest.trim_start_matches([' ', '\t']);
                if r.len() == rest.len() {
                    return Err(no_match());
                }
                r
            }
        };
        let (hour, rest) = take_digits(rest, 1, 2).ok_or_else(no_match)?;
        let rest = rest.strip_prefix(':').ok_or_else(no_match)?;
        let (minute, rest) = take_digits(rest, 2, 2).ok_or_else(no_match)?;
        let rest = rest.strip_prefix(':').ok_or_else(no_match)?;
        let (second, mut rest) = take_digits(rest, 2, 2).ok_or_else(no_match)?;

        // (?:\.(?P<fraction>[0-9]*))?
        let mut microsecond = 0;
        if let Some(after_dot) = rest.strip_prefix('.') {
            let n = after_dot.bytes().take_while(u8::is_ascii_digit).count();
            let fraction = &after_dot[..n];
            rest = &after_dot[n..];
            if !fraction.is_empty() {
                // `fraction[:6]` right-padded with zeros to six digits.
                let kept = &fraction[..fraction.len().min(6)];
                microsecond = digits(kept) * 10u32.pow(6 - kept.len() as u32);
            }
        }

        // (?:[ \t]*(?P<tz>Z|(?P<tz_sign>[-+])(?P<tz_hour>[0-9][0-9]?)(?::(?P<tz_minute>[0-9][0-9]))?))?
        let mut utc_offset_minutes = None;
        if !rest.is_empty() {
            let tz = rest.trim_start_matches([' ', '\t']);
            if tz == "Z" {
                utc_offset_minutes = Some(0);
            } else {
                let sign: i32 = match tz.as_bytes().first() {
                    Some(b'-') => -1,
                    Some(b'+') => 1,
                    _ => return Err(no_match()),
                };
                let (tz_hour, tz_rest) = take_digits(&tz[1..], 1, 2).ok_or_else(no_match)?;
                let tz_minute = if tz_rest.is_empty() {
                    ""
                } else {
                    let m = tz_rest.strip_prefix(':').ok_or_else(no_match)?;
                    if m.len() != 2 || !m.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(no_match());
                    }
                    m
                };
                let minutes = digits(tz_hour) as i32 * 60 + digits(tz_minute) as i32;
                if minutes >= 24 * 60 {
                    return Err(
                        "offset must be a timedelta strictly between -timedelta(hours=24) \
                         and timedelta(hours=24)"
                            .to_owned(),
                    );
                }
                utc_offset_minutes = Some(sign * minutes);
            }
        }

        let (hour, minute, second) = (digits(hour), digits(minute), digits(second));
        if hour > 23 {
            return Err("hour must be in 0..23".to_owned());
        }
        if minute > 59 {
            return Err("minute must be in 0..59".to_owned());
        }
        if second > 59 {
            return Err("second must be in 0..59".to_owned());
        }
        Ok(Temporal::DateTime(DateTime {
            date,
            hour: hour as u8,
            minute: minute as u8,
            second: second as u8,
            microsecond,
            utc_offset_minutes,
        }))
    }
}

/// Split off between `min` and `max` leading ASCII digits (greedy).
fn take_digits(s: &str, min: usize, max: usize) -> Option<(&str, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count().min(max);
    (n >= min).then(|| s.split_at(n))
}

impl fmt::Display for Date {
    /// `date.isoformat()` (= `str(date)`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl fmt::Display for DateTime {
    /// `datetime.isoformat(' ')` (= `str(datetime)`): `YYYY-MM-DD HH:MM:SS`,
    /// `.ffffff` when the microsecond is non-zero, `+HH:MM` for an aware one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {:02}:{:02}:{:02}",
            self.date, self.hour, self.minute, self.second
        )?;
        if self.microsecond != 0 {
            write!(f, ".{:06}", self.microsecond)?;
        }
        if let Some(offset) = self.utc_offset_minutes {
            let sign = if offset < 0 { '-' } else { '+' };
            write!(f, "{sign}{:02}:{:02}", offset.abs() / 60, offset.abs() % 60)?;
        }
        Ok(())
    }
}

impl fmt::Display for Temporal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Temporal::Date(d) => d.fmt(f),
            Temporal::DateTime(dt) => dt.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(text: &str) -> String {
        Temporal::from_yaml_timestamp(text).unwrap().to_string()
    }

    // Expected strings from `str(yaml.safe_load(text))` under PyYAML 6.0.3.
    #[test]
    fn str_matches_python() {
        assert_eq!(show("2024-01-01"), "2024-01-01");
        assert_eq!(show("2024-1-5"), "2024-01-05");
        assert_eq!(show("2024-01-01T10:30:00Z"), "2024-01-01 10:30:00+00:00");
        assert_eq!(show("2024-01-01 1:02:03"), "2024-01-01 01:02:03");
        assert_eq!(
            show("2001-12-14t21:59:43.10-05:00"),
            "2001-12-14 21:59:43.100000-05:00"
        );
        assert_eq!(
            show("2024-01-01 10:00:00.1234567 +5"),
            "2024-01-01 10:00:00.123456+05:00"
        );
        assert_eq!(show("2024-01-01 10:00:00."), "2024-01-01 10:00:00");
    }

    #[test]
    fn repr_matches_python() {
        let r = |t: &str| Temporal::from_yaml_timestamp(t).unwrap().repr();
        assert_eq!(r("2024-01-01"), "datetime.date(2024, 1, 1)");
        assert_eq!(
            r("2024-01-01 10:00:00Z"),
            "datetime.datetime(2024, 1, 1, 10, 0, tzinfo=datetime.timezone.utc)"
        );
        assert_eq!(
            r("2024-01-01 10:00:05.25-05:00"),
            "datetime.datetime(2024, 1, 1, 10, 0, 5, 250000, \
             tzinfo=datetime.timezone(datetime.timedelta(days=-1, seconds=68400)))"
        );
        assert_eq!(
            r("2024-01-01 10:00:00"),
            "datetime.datetime(2024, 1, 1, 10, 0)"
        );
    }

    #[test]
    fn invalid_fields_are_errors() {
        for bad in [
            "2024-13-01",
            "2023-02-29",
            "0000-01-01",
            "2024-01-01 25:00:00",
            "2024-01-01 10:60:00",
            "2024-01-01 10:00:00+24:00",
        ] {
            assert!(Temporal::from_yaml_timestamp(bad).is_err(), "{bad}");
        }
        assert_eq!(show("2024-02-29"), "2024-02-29");
    }
}

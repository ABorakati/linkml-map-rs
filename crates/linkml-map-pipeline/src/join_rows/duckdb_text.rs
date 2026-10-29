//! The date, time and UUID half of DuckDB's `read_json_auto` string detection (DuckDB 1.5.x).
//!
//! `read_json_auto` turns a column of JSON strings into `DATE`, `TIMESTAMP`, `TIME` or `UUID` when every
//! sampled string parses as that type, and `CAST(... AS VARCHAR)` then prints the value in DuckDB's own
//! text form (`2020-01-01T10:00:00Z` reads back as `2020-01-01 10:00:00`).  This module ports the parsers
//! and printers involved, keeping their names where there is a counterpart:
//!
//! * `StrpTimeFormat::Parse` (`src/function/scalar/strftime_format.cpp`), for the fixed format templates of
//!   `JSONScanData::InitializeFormats` (`extension/json/json_scan.cpp`): six `DATE` and twelve `TIMESTAMP`
//!   formats, tried from the last to the first;
//! * `Time::TryConvertTime` (`src/common/types/time.cpp`, strict), the `TIME` candidate;
//! * `BaseUUID::FromString` (`src/common/types/uuid.cpp`, strict), the `UUID` candidate;
//! * `Date::ToString`, `Time::ToString` and `Timestamp::ToString` for the text of each.
//!
//! Strings are handled as bytes, as DuckDB does; a read past the end yields NUL, as it does in C++.

/// `Interval::MICROS_PER_DAY`.
const MICROS_PER_DAY: i64 = 86_400_000_000;

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// A byte string that reads as NUL past its end (a C string).
struct CStr<'a>(&'a [u8]);

impl CStr<'_> {
    fn at(&self, pos: usize) -> u8 {
        self.0.get(pos).copied().unwrap_or(0)
    }
}

// ─── calendar ────────────────────────────────────────────────────────────────

/// `Date::IsLeapYear`.
fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// `Date::IsValid` for the years a four-digit `%Y` can produce.
fn date_is_valid(year: i32, month: i32, day: i32) -> bool {
    if !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let days = match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    day <= days
}

/// Days since 1970-01-01 of a proleptic Gregorian date (astronomical year numbering, as DuckDB's).
fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date::Convert`: the inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `Date::ToString` (`DateToStringCast`): `YYYY-MM-DD`, with ` (BC)` for year 0 and below.
pub(super) fn date_text(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    let (year, bc) = if year <= 0 {
        (1 - year, " (BC)")
    } else {
        (year, "")
    };
    format!("{year:04}-{month:02}-{day:02}{bc}")
}

/// `Time::ToString` (`TimeToStringCast`): `HH:MM:SS`, microseconds without trailing zeros.
pub(super) fn time_text(micros: i64) -> String {
    let hour = micros / 3_600_000_000;
    let minute = micros % 3_600_000_000 / 60_000_000;
    let second = micros % 60_000_000 / 1_000_000;
    let fraction = micros % 1_000_000;
    let mut out = format!("{hour:02}:{minute:02}:{second:02}");
    if fraction != 0 {
        out.push('.');
        out.push_str(format!("{fraction:06}").trim_end_matches('0'));
    }
    out
}

/// `Timestamp::ToString`.
pub(super) fn timestamp_text(micros: i64) -> String {
    let days = micros.div_euclid(MICROS_PER_DAY);
    format!(
        "{} {}",
        date_text(days),
        time_text(micros.rem_euclid(MICROS_PER_DAY))
    )
}

// ─── strptime ────────────────────────────────────────────────────────────────

/// The `StrTimeSpecifier`s the format templates use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spec {
    /// `%Y`
    Year,
    /// `%y`
    YearShort,
    /// `%m`
    Month,
    /// `%d`
    Day,
    /// `%H`
    Hour24,
    /// `%I`
    Hour12,
    /// `%M`
    Minute,
    /// `%S`
    Second,
    /// `%f`
    Micros,
    /// `%p`
    AmPm,
    /// `%z`
    UtcOffset,
}

impl Spec {
    /// `StrpTimeFormat::NumericSpecifierWidth`; `None` for the non-numeric specifiers.
    fn width(self) -> Option<usize> {
        match self {
            Self::Year => Some(4),
            Self::YearShort
            | Self::Month
            | Self::Day
            | Self::Hour24
            | Self::Hour12
            | Self::Minute
            | Self::Second => Some(2),
            Self::Micros => Some(6),
            Self::AmPm | Self::UtcOffset => None,
        }
    }
}

/// A compiled `StrpTimeFormat`: `literals[i]` precedes `specs[i]`, the last literal follows the last spec.
pub(super) struct StrpTimeFormat {
    literals: Vec<Vec<u8>>,
    specs: Vec<Spec>,
}

/// `StrpTimeFormat::ParseResult`.
struct ParseResult {
    /// year, month, day, hour, minute, second, nanosecond, UTC offset in seconds
    data: [i32; 8],
}

impl StrpTimeFormat {
    /// `StrTimeFormat::ParseFormatSpecifier` for the specifiers the templates use.
    fn new(format: &str) -> Self {
        let mut literals = vec![Vec::new()];
        let mut specs = Vec::new();
        let mut bytes = format.bytes();
        while let Some(b) = bytes.next() {
            if b != b'%' {
                literals.last_mut().expect("one literal").push(b);
                continue;
            }
            specs.push(match bytes.next() {
                Some(b'Y') => Spec::Year,
                Some(b'y') => Spec::YearShort,
                Some(b'm') => Spec::Month,
                Some(b'd') => Spec::Day,
                Some(b'H') => Spec::Hour24,
                Some(b'I') => Spec::Hour12,
                Some(b'M') => Spec::Minute,
                Some(b'S') => Spec::Second,
                Some(b'f') => Spec::Micros,
                Some(b'p') => Spec::AmPm,
                Some(b'z') => Spec::UtcOffset,
                other => unreachable!("format specifier {other:?} is not in the JSON templates"),
            });
            literals.push(Vec::new());
        }
        Self { literals, specs }
    }

    /// `StrpTimeFormat::Parse` (non-strict).
    #[allow(clippy::too_many_lines)] // one arm per specifier, as upstream
    fn parse(&self, input: &[u8]) -> Option<ParseResult> {
        let mut data = [1900, 1, 1, 0, 0, 0, 0, 0];
        // skip leading spaces
        let lead = input.iter().take_while(|b| is_space(**b)).count();
        let text = CStr(&input[lead..]);
        let size = input.len() - lead;
        // Specials (`infinity`, `epoch`): only the trailing characters are checked, the date stays 1900-01-01
        // (`TryToDate` does not look at `is_special`).
        if size > 4 {
            let first = text.at(0);
            let rest = &input[lead..];
            let special = if first.is_ascii_alphabetic() {
                try_convert_date_special(rest, "infinity", 3)
                    .or_else(|| try_convert_date_special(rest, "epoch", 5))
            } else if first == b'-' {
                try_convert_date_special(rest, "-infinity", 4)
            } else {
                None
            };
            if let Some(mut pos) = special {
                while pos < size && is_space(text.at(pos)) {
                    pos += 1;
                }
                return (pos == size).then_some(ParseResult { data });
            }
        }
        let mut pos = 0usize;
        let mut ampm = None::<bool>; // Some(true) = PM
        for i in 0.. {
            // the literal first; a run of spaces matches a run of spaces
            let literal = &self.literals[i];
            let mut l = 0;
            while l < literal.len() {
                if is_space(literal[l]) {
                    if !is_space(text.at(pos)) {
                        return None;
                    }
                    pos += 1;
                    while pos < size && is_space(text.at(pos)) {
                        pos += 1;
                    }
                    l += 1;
                    while l < literal.len() && is_space(literal[l]) {
                        l += 1;
                    }
                    continue;
                }
                let got = text.at(pos);
                pos += 1;
                if got != literal[l] {
                    return None;
                }
                l += 1;
            }
            let Some(&spec) = self.specs.get(i) else {
                break;
            };
            if let Some(width) = spec.width() {
                let start = pos;
                let end = start + width;
                let mut number: u64 = 0;
                let mut digits = 0usize;
                while pos < size && pos < end && is_digit(text.at(pos)) {
                    number = number * 10 + u64::from(text.at(pos) - b'0');
                    pos += 1;
                    digits += 1;
                }
                if pos == start {
                    return None;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let small = number as i32;
                match spec {
                    Spec::Day => {
                        if !(1..=31).contains(&number) {
                            return None;
                        }
                        data[2] = small;
                    }
                    Spec::Month => {
                        if !(1..=12).contains(&number) {
                            return None;
                        }
                        data[1] = small;
                    }
                    Spec::YearShort => {
                        // Python's pivot: 69 and above is 19xx
                        data[0] = if number >= 69 {
                            1900 + small
                        } else {
                            2000 + small
                        };
                    }
                    Spec::Year => data[0] = small,
                    Spec::Hour24 => {
                        if number >= 24 {
                            return None;
                        }
                        data[3] = small;
                    }
                    Spec::Hour12 => {
                        if !(1..=12).contains(&number) {
                            return None;
                        }
                        data[3] = small;
                    }
                    Spec::Minute => {
                        if number >= 60 {
                            return None;
                        }
                        data[4] = small;
                    }
                    Spec::Second => {
                        if number >= 60 {
                            return None;
                        }
                        data[5] = small;
                    }
                    Spec::Micros => {
                        for _ in digits..width {
                            number *= 10;
                        }
                        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                        let micros = number as i32;
                        data[6] = micros * 1000;
                    }
                    Spec::AmPm | Spec::UtcOffset => unreachable!("not numeric"),
                }
            } else if spec == Spec::AmPm {
                if pos + 2 > size {
                    return None;
                }
                let pa = text.at(pos).to_ascii_lowercase();
                if !text.at(pos + 1).eq_ignore_ascii_case(&b'm') {
                    return None;
                }
                ampm = Some(match pa {
                    b'p' => true,
                    b'a' => false,
                    _ => return None,
                });
                pos += 2;
            } else {
                let (hh, mm, ss) = try_parse_utc_offset(&text, &mut pos, size)?;
                data[7] = (hh * 60 + mm) * 60 + ss;
            }
        }
        while pos < size && is_space(text.at(pos)) {
            pos += 1;
        }
        if pos != size {
            return None;
        }
        if let Some(pm) = ampm {
            if data[3] > 12 {
                return None;
            }
            if pm {
                if data[3] != 12 {
                    data[3] += 12;
                }
            } else if data[3] == 12 {
                data[3] = 0;
            }
        }
        Some(ParseResult { data })
    }

    /// `StrpTimeFormat::TryParseDate`: days since the epoch.
    pub(super) fn try_parse_date(&self, input: &[u8]) -> Option<i64> {
        self.parse(input)?.try_to_date()
    }

    /// `StrpTimeFormat::TryParseTimestamp`: microseconds since the epoch.
    pub(super) fn try_parse_timestamp(&self, input: &[u8]) -> Option<i64> {
        self.parse(input)?.try_to_timestamp()
    }
}

impl ParseResult {
    /// `ParseResult::TryToDate`.
    fn try_to_date(&self) -> Option<i64> {
        let [year, month, day, ..] = self.data;
        date_is_valid(year, month, day).then(|| days_from_civil(year, month, day))
    }

    /// `ParseResult::TryToTimestamp`: the UTC offset is subtracted from the time, which may leave the day.
    fn try_to_timestamp(&self) -> Option<i64> {
        let days = self.try_to_date()?;
        let [_, _, _, hour, minute, second, nanos, offset] = self.data;
        // `StrfTimeSplitOffset` splits a signed offset into signed parts; subtracting them is subtracting it.
        let micros =
            (i64::from(hour) * 60 + i64::from(minute)) * 60 + i64::from(second) - i64::from(offset);
        let micros = micros * 1_000_000 + i64::from((nanos + 500) / 1000);
        days.checked_mul(MICROS_PER_DAY)?.checked_add(micros)
    }
}

/// `Date::TryConvertDateSpecial`: `special` matched case-insensitively from the start of `buf`; the end
/// position when it matches (in full, or as the abbreviation when the string ends there).
fn try_convert_date_special(buf: &[u8], special: &str, abbr: usize) -> Option<usize> {
    let special = special.as_bytes();
    let mut p = 0;
    while p < buf.len() && p < special.len() {
        if buf[p].to_ascii_lowercase() != special[p] {
            return None;
        }
        p += 1;
    }
    (p == special.len() || p == abbr).then_some(p)
}

/// `Timestamp::TryParseUTCOffset`: `±HH[[:]MM[:SS]]`; the parts carry the sign.
fn try_parse_utc_offset(text: &CStr<'_>, pos: &mut usize, len: usize) -> Option<(i32, i32, i32)> {
    let mut cur = *pos;
    if cur + 3 > len {
        return None;
    }
    let sign = text.at(cur);
    if sign != b'+' && sign != b'-' {
        return None;
    }
    let apply = |n: i32| if sign == b'-' { -n } else { n };
    cur += 1;
    if !is_digit(text.at(cur)) || !is_digit(text.at(cur + 1)) {
        return None;
    }
    let two = |at: usize| i32::from(text.at(at) - b'0') * 10 + i32::from(text.at(at + 1) - b'0');
    let hh = apply(two(cur));
    cur += 2;
    let (mut mm, mut ss) = (0, 0);
    'parts: {
        if cur >= len {
            break 'parts;
        }
        let colons = text.at(cur) == b':';
        if colons {
            cur += 1;
        }
        if cur + 2 > len || !is_digit(text.at(cur)) || !is_digit(text.at(cur + 1)) {
            break 'parts;
        }
        mm = apply(two(cur));
        cur += 2;
        if cur >= len || !colons || text.at(cur) != b':' {
            break 'parts;
        }
        cur += 1;
        if cur + 2 > len || !is_digit(text.at(cur)) || !is_digit(text.at(cur + 1)) {
            break 'parts;
        }
        ss = apply(two(cur));
        cur += 2;
    }
    *pos = cur;
    Some((hh, mm, ss))
}

/// `JSONScanData::InitializeFormats`: the `DATE` templates, in DuckDB's order.
pub(super) fn date_formats() -> Vec<StrpTimeFormat> {
    [
        "%m-%d-%Y", "%m-%d-%y", "%d-%m-%Y", "%d-%m-%y", "%Y-%m-%d", "%y-%m-%d",
    ]
    .into_iter()
    .map(StrpTimeFormat::new)
    .collect()
}

/// `JSONScanData::InitializeFormats`: the `TIMESTAMP` templates, in DuckDB's order.
pub(super) fn timestamp_formats() -> Vec<StrpTimeFormat> {
    [
        "%Y-%m-%d %H:%M:%S.%f",
        "%m-%d-%Y %I:%M:%S %p",
        "%m-%d-%y %I:%M:%S %p",
        "%d-%m-%Y %H:%M:%S",
        "%d-%m-%y %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S.%fZ",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S.%f%z",
    ]
    .into_iter()
    .map(StrpTimeFormat::new)
    .collect()
}

// ─── TIME and UUID ───────────────────────────────────────────────────────────

/// `Date::ParseDoubleDigit`.
fn parse_double_digit(buf: &CStr<'_>, len: usize, pos: &mut usize) -> Option<i32> {
    if *pos < len && is_digit(buf.at(*pos)) {
        let mut result = i32::from(buf.at(*pos) - b'0');
        *pos += 1;
        if *pos < len && is_digit(buf.at(*pos)) {
            result = i32::from(buf.at(*pos) - b'0') + result * 10;
            *pos += 1;
        }
        return Some(result);
    }
    None
}

/// `Time::TryConvertInternal`: the time of day in microseconds (hours may exceed 24: intervals) and the
/// position after it.  Non-strict, a missing minute or second reads as zero and the rest of the string is
/// left alone.
fn try_convert_internal(input: &[u8], strict: bool) -> Option<(i64, usize)> {
    let buf = CStr(input);
    let len = input.len();
    let mut pos = 0;
    while pos < len && is_space(buf.at(pos)) {
        pos += 1;
    }
    if pos >= len || !is_digit(buf.at(pos)) {
        return None;
    }
    // up to nine digits of hours, to support intervals
    let mut hour: i64 = 0;
    let mut digits = 9;
    while pos < len && is_digit(buf.at(pos)) {
        if digits > 0 {
            hour = hour * 10 + i64::from(buf.at(pos) - b'0');
            digits -= 1;
        } else {
            return None;
        }
        pos += 1;
    }
    if pos >= len {
        return None;
    }
    let sep = buf.at(pos);
    pos += 1;
    if sep != b':' {
        return None;
    }
    let sep_pos = pos;
    let min = if pos == len && !strict {
        0
    } else {
        let min = parse_double_digit(&buf, len, &mut pos)?;
        if !(0..60).contains(&min) {
            return None;
        }
        min
    };
    if pos > len {
        return None;
    }
    let sec = if pos == len && (!strict || sep_pos + 2 == pos) {
        0
    } else {
        let next = buf.at(pos);
        pos += 1;
        if next != sep {
            return None;
        }
        if pos == len && !strict {
            0
        } else {
            let sec = parse_double_digit(&buf, len, &mut pos)?;
            if !(0..60).contains(&sec) {
                return None;
            }
            sec
        }
    };
    let mut micros: i64 = 0;
    if pos < len && buf.at(pos) == b'.' {
        pos += 1;
        let mut mult = 100_000;
        while pos < len && is_digit(buf.at(pos)) {
            if mult > 0 {
                micros += i64::from(buf.at(pos) - b'0') * mult;
            }
            pos += 1;
            mult /= 10;
        }
    }
    if strict {
        while pos < len && is_space(buf.at(pos)) {
            pos += 1;
        }
        if pos < len {
            return None;
        }
    }
    let total = ((hour * 60 + i64::from(min)) * 60 + i64::from(sec)) * 1_000_000 + micros;
    Some((total, pos))
}

/// `Time::TryConvertTime` with `strict = true`: microseconds since midnight.  This is the `TIME` candidate of
/// type detection.
pub(super) fn try_convert_time(input: &[u8]) -> Option<i64> {
    let (micros, _) = try_convert_internal(input, true)?;
    (micros <= MICROS_PER_DAY).then_some(micros)
}

/// `Time::TryConvertTime` with `strict = false`, the cast of a JSON string to a `TIME` column: a time, or
/// failing that the time of day of a timestamp (`2020-01-02 11:00:00` reads as `11:00:00`).
pub(super) fn try_convert_time_lenient(input: &[u8]) -> Option<i64> {
    match try_convert_internal(input, false) {
        Some((micros, _)) => (micros <= MICROS_PER_DAY).then_some(micros),
        None => {
            let ts = try_convert_timestamp(input)?;
            Some(ts.rem_euclid(MICROS_PER_DAY))
        }
    }
}

/// `Date::TryConvertDate` with `strict = false`: days since the epoch and the position after the date, or
/// `None` for a string that is not a date, or for the specials (`infinity`, `-infinity`), which are not a
/// finite day.
fn try_convert_date(input: &[u8]) -> Option<(i64, usize)> {
    let buf = CStr(input);
    let len = input.len();
    let mut pos = 0;
    while pos < len && is_space(buf.at(pos)) {
        pos += 1;
    }
    if pos >= len {
        return None;
    }
    let mut year_negative = false;
    if buf.at(pos) == b'-' {
        year_negative = true;
        pos += 1;
        if pos >= len {
            return None;
        }
    }
    if !is_digit(buf.at(pos)) {
        // `epoch` is a day; `infinity` is not a finite one
        let rest = &input[pos..];
        if try_convert_date_special(rest, "infinity", 3).is_some() {
            return None;
        }
        let end = try_convert_date_special(rest, "epoch", 5)?;
        pos += end;
        while pos < len && is_space(buf.at(pos)) {
            pos += 1;
        }
        return (pos == len).then_some((0, pos));
    }
    let mut year: i32 = 0;
    while pos < len && is_digit(buf.at(pos)) {
        if year >= 100_000_000 {
            return None;
        }
        year = i32::from(buf.at(pos) - b'0') + year * 10;
        pos += 1;
    }
    if year_negative {
        year = -year;
    }
    if pos >= len {
        return None;
    }
    let sep = buf.at(pos);
    pos += 1;
    if !matches!(sep, b' ' | b'-' | b'/' | b'\\') {
        return None;
    }
    let month = parse_double_digit(&buf, len, &mut pos)?;
    if pos >= len {
        return None;
    }
    let next = buf.at(pos);
    pos += 1;
    if next != sep || pos >= len {
        return None;
    }
    let day = parse_double_digit(&buf, len, &mut pos)?;
    // an optional trailing " (BC)"
    if len - pos >= 5
        && is_space(buf.at(pos))
        && buf.at(pos + 1) == b'('
        && buf.at(pos + 2).eq_ignore_ascii_case(&b'b')
        && buf.at(pos + 3).eq_ignore_ascii_case(&b'c')
        && buf.at(pos + 4) == b')'
    {
        if year_negative || year == 0 {
            return None;
        }
        year = -year + 1;
        pos += 5;
    }
    // non-strict: only a digit right after the date is an error
    if pos < len && is_digit(buf.at(pos)) {
        return None;
    }
    date_is_valid(year, month, day).then(|| (days_from_civil(year, month, day), pos))
}

/// `Timestamp::TryConvertTimestamp` (`use_offset = false`): microseconds since the epoch of a date, with a
/// time of day, a `Z` or an offset, or a `UTC` zone after it.
fn try_convert_timestamp(input: &[u8]) -> Option<i64> {
    let buf = CStr(input);
    let len = input.len();
    let (days, mut pos) = try_convert_date(input)?;
    let from_datetime = |micros: i64| days.checked_mul(MICROS_PER_DAY)?.checked_add(micros);
    if pos == len {
        return from_datetime(0);
    }
    if buf.at(pos) == b' ' || buf.at(pos) == b'T' {
        pos += 1;
    }
    let (time, used) = try_convert_internal(&input[pos.min(len)..], false)?;
    if time > MICROS_PER_DAY {
        return None;
    }
    pos += used;
    let result = from_datetime(time)?;
    if pos < len {
        let mut cur = pos;
        if buf.at(pos) == b'Z' {
            cur += 1;
        } else if try_parse_utc_offset(&buf, &mut cur, len).is_none() {
            // a time zone name: only UTC is known without ICU
            if buf.at(cur) != b' ' {
                return None;
            }
            cur += 1;
            let start = cur;
            while cur < len
                && (buf.at(cur).is_ascii_alphanumeric() || matches!(buf.at(cur), b'/' | b'_'))
            {
                cur += 1;
            }
            let name = &input[start..cur];
            if !name.is_empty() && !name.eq_ignore_ascii_case(b"utc") {
                return None;
            }
        }
        while cur < len && is_space(buf.at(cur)) {
            cur += 1;
        }
        if cur < len {
            return None;
        }
    }
    Some(result)
}

/// `BaseUUID::FromString` followed by `BaseUUID::ToString`: the canonical lower-case text, or `None` when the
/// string is not a UUID.  Strict (type detection) wants 36 characters with the four hyphens in place;
/// otherwise (the cast of a later record) hyphens may be anywhere and braces are allowed.
pub(super) fn uuid_text(input: &[u8], strict: bool) -> Option<String> {
    let front = *input.first()?;
    let braces = usize::from(front == b'{');
    if braces == 1 && input.last() != Some(&b'}') {
        return None;
    }
    if strict && (input.len() != 36 || [8, 13, 18, 23].iter().any(|&i| input[i] != b'-')) {
        return None;
    }
    let mut hex = String::with_capacity(32);
    for &b in input.get(braces..input.len().saturating_sub(braces))? {
        if b == b'-' {
            continue;
        }
        if hex.len() >= 32 || !b.is_ascii_hexdigit() {
            return None;
        }
        hex.push(char::from(b.to_ascii_lowercase()));
    }
    (hex.len() == 32).then(|| {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(format: usize, text: &str) -> Option<String> {
        timestamp_formats()[format]
            .try_parse_timestamp(text.as_bytes())
            .map(timestamp_text)
    }

    #[test]
    fn calendar_round_trips() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (1, 1, 1),
            (0, 12, 31),
            (9999, 12, 31),
            (1899, 7, 4),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(
                civil_from_days(days),
                (i64::from(y), i64::from(m), i64::from(d))
            );
        }
        assert_eq!(date_text(days_from_civil(0, 3, 1)), "0001-03-01 (BC)");
        assert_eq!(date_text(days_from_civil(12, 3, 1)), "0012-03-01");
    }

    #[test]
    fn timestamps_apply_offsets_and_trim_fractions() {
        assert_eq!(
            ts(8, "2020-01-01T10:00:00Z").as_deref(),
            Some("2020-01-01 10:00:00")
        );
        assert_eq!(
            ts(10, "2020-01-01T10:00:00+02:00").as_deref(),
            Some("2020-01-01 08:00:00")
        );
        assert_eq!(
            ts(11, "2020-01-01T10:00:00.5-0130").as_deref(),
            Some("2020-01-01 11:30:00.5")
        );
        assert_eq!(
            ts(0, "2020-01-01 10:00:00.123456").as_deref(),
            Some("2020-01-01 10:00:00.123456")
        );
        assert_eq!(
            ts(1, "01-02-2020 12:00:00 am").as_deref(),
            Some("2020-01-02 00:00:00")
        );
        assert_eq!(ts(9, "2020-01-01T10:00:00.123456789Z"), None);
    }

    #[test]
    fn times_and_uuids() {
        assert_eq!(
            try_convert_time(b"10:00").map(time_text).as_deref(),
            Some("10:00:00")
        );
        assert_eq!(
            try_convert_time(b"24:00:00").map(time_text).as_deref(),
            Some("24:00:00")
        );
        assert_eq!(
            try_convert_time(b"10:00:00.1234567")
                .map(time_text)
                .as_deref(),
            Some("10:00:00.123456")
        );
        assert_eq!(try_convert_time(b"1:2"), None);
        assert_eq!(try_convert_time(b"24:00:01"), None);
        assert_eq!(
            uuid_text(b"7F0Dcad7-862E-5CeC-4D8e-A544eb9c736F", true).as_deref(),
            Some("7f0dcad7-862e-5cec-4d8e-a544eb9c736f")
        );
        assert_eq!(uuid_text(b"7F0Dcad7862E5CeC4D8eA544eb9c736F", true), None);
        assert!(uuid_text(b"7F0Dcad7862E5CeC4D8eA544eb9c736F", false).is_some());
    }
}

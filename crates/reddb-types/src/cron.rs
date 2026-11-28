//! Five-field cron rules evaluated against a bundled IANA time zone database.
use jiff::{civil::Date, tz::TimeZone, Timestamp};
use std::fmt;
use std::hash::{Hash, Hasher};

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;
// Gregorian dates and weekdays repeat every 400 years. The extra days cover
// offset changes between the endpoints of that civil calendar cycle.
const SEARCH_MS: i64 = (146_097 + 2) * DAY_MS;
const MAX_EXPRESSION_LEN: usize = 256;
const MAX_ZONE_LEN: usize = 64;
pub const MAX_ENCODED_LEN: usize = MAX_EXPRESSION_LEN + MAX_ZONE_LEN + 3;

#[derive(Debug, Clone)]
pub struct CronSchedule {
    encoded: Box<str>,
    zone: TimeZone,
    minute: u64,
    hour: u64,
    day: u64,
    month: u64,
    weekday: u64,
    day_and_weekday: bool,
    possible_date: bool,
}

impl PartialEq for CronSchedule {
    fn eq(&self, other: &Self) -> bool {
        self.encoded == other.encoded
    }
}

impl Eq for CronSchedule {}

impl Hash for CronSchedule {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.encoded.hash(state);
    }
}

impl fmt::Display for CronSchedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CronSchedule {
    /// Parse a bare five-field expression with an explicit IANA zone.
    pub fn new(expression: &str, timezone: &str) -> Result<Self, String> {
        if expression.len() > MAX_EXPRESSION_LEN {
            return Err("cron expression is too long".into());
        }
        let mut fields = expression.split_ascii_whitespace();
        let [Some(minute), Some(hour), Some(day), Some(month), Some(weekday), None] =
            std::array::from_fn(|_| fields.next())
        else {
            return Err("cron requires five fields: minute hour day month weekday".into());
        };
        let zone = timezone_from_name(timezone)?;
        let name = zone.iana_name().unwrap_or("UTC");
        let weekday_mask = parse_field(weekday, 0, 7, WEEKDAYS)?;
        // Sunday may be written as either 0 or 7.
        let sunday = (weekday_mask >> 7) & 1;
        let encoded = format!("{minute} {hour} {day} {month} {weekday} [{name}]");
        let day_and_weekday = day.starts_with('*') || weekday.starts_with('*');
        let day = parse_field(day, 1, 31, &[])?;
        let month = parse_field(month, 1, 12, MONTHS)?;
        let possible_date = !day_and_weekday
            || MAX_DAYS_BY_MONTH.iter().enumerate().any(|(index, &days)| {
                month & (1 << (index + 1)) != 0 && day & ((1 << (days + 1)) - 1) != 0
            });
        Ok(Self {
            encoded: encoded.into(),
            zone,
            minute: parse_field(minute, 0, 59, &[])?,
            hour: parse_field(hour, 0, 23, &[])?,
            day,
            month,
            weekday: (weekday_mask & 0x7f) | sunday,
            day_and_weekday,
            possible_date,
        })
    }

    /// Accept the lossless text representation or a bare expression.
    pub fn parse(input: &str, default_timezone: &str) -> Result<Self, String> {
        if let Some((expression, zone)) = input.strip_suffix(']').and_then(|s| s.rsplit_once(" ["))
        {
            return Self::new(expression, zone);
        }
        Self::new(input, default_timezone)
    }

    /// Decode a persisted rule. Its effective time zone is mandatory.
    pub fn from_encoded(input: &str) -> Result<Self, String> {
        let (expression, zone) = input
            .strip_suffix(']')
            .and_then(|s| s.rsplit_once(" ["))
            .ok_or_else(|| "stored cron rule requires a time zone".to_string())?;
        Self::new(expression, zone)
    }

    pub fn as_str(&self) -> &str {
        &self.encoded
    }

    /// Find the first scheduled instant at or after the supplied instant.
    pub fn next(&self, start_ms: i64) -> Result<Option<i64>, String> {
        self.first_in_window(start_ms, None)
    }

    /// Test the half-open interval [start, end).
    pub fn overlaps(&self, start_ms: i64, end_ms: i64) -> Result<bool, String> {
        if end_ms < start_ms {
            return Err("cron window end precedes its start".into());
        }
        Ok(self.first_in_window(start_ms, Some(end_ms))?.is_some())
    }

    /// Scheduled instants in [start_ms, end_ms), in absolute time order.
    pub fn occurrences(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<impl Iterator<Item = Result<i64, String>> + '_, String> {
        if end_ms < start_ms {
            return Err("cron window end precedes its start".into());
        }
        let mut cursor = Some(start_ms);
        Ok(std::iter::from_fn(move || {
            let start = cursor.take()?;
            match self.first_in_window(start, Some(end_ms)) {
                Ok(Some(instant)) => {
                    // Offsets can change by sub-minute amounts in historical
                    // IANA data. Resume one millisecond after the match.
                    cursor = Some(instant + 1);
                    Some(Ok(instant))
                }
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        }))
    }

    fn first_in_window(&self, start_ms: i64, end_ms: Option<i64>) -> Result<Option<i64>, String> {
        let timestamp = |ms| Timestamp::from_millisecond(ms).map_err(|error| error.to_string());
        let start = timestamp(start_ms)?;
        let end = match end_ms {
            Some(ms) => timestamp(ms)?,
            None => Timestamp::MAX,
        };
        let end_ms = end.as_millisecond().min(start_ms + SEARCH_MS);
        if end_ms <= start_ms || !self.possible_date {
            return Ok(None);
        }

        // Search each constant-offset segment in instant order. This naturally
        // skips local gaps and visits each side of a local fold independently.
        let mut segment_start = start;
        for transition in self.zone.following(start) {
            let boundary = transition.timestamp();
            let segment_end = boundary.as_millisecond().min(end_ms);
            if let Some(found) = self.first_in_segment(segment_start, segment_end)? {
                return Ok(Some(found));
            }
            if boundary.as_millisecond() >= end_ms {
                return Ok(None);
            }
            segment_start = boundary;
        }
        self.first_in_segment(segment_start, end_ms)
    }

    fn first_in_segment(&self, start: Timestamp, end_ms: i64) -> Result<Option<i64>, String> {
        let offset_ms = i64::from(self.zone.to_offset(start).seconds()) * 1000;
        let local_start = start.as_millisecond() + offset_ms;
        let local_end = end_ms + offset_ms;
        let mut day_start = local_start.div_euclid(DAY_MS) * DAY_MS;
        let mut date = self.zone.to_datetime(start).date();
        while day_start < local_end {
            if self.matches_date(date) {
                let in_day = local_start.saturating_sub(day_start).max(0);
                let first_minute = ((in_day + MINUTE_MS - 1) / MINUTE_MS) as u32;
                let first_hour = first_minute / 60;
                let mut hours = self.hour & (u64::MAX << first_hour);
                while hours != 0 {
                    let hour = hours.trailing_zeros();
                    let minutes = if hour == first_hour {
                        self.minute & (u64::MAX << (first_minute % 60))
                    } else {
                        self.minute
                    };
                    if minutes != 0 {
                        let local =
                            day_start + i64::from(hour * 60 + minutes.trailing_zeros()) * MINUTE_MS;
                        return Ok((local < local_end).then_some(local - offset_ms));
                    }
                    hours &= hours - 1;
                }
            }
            day_start += DAY_MS;
            if day_start < local_end {
                date = date.tomorrow().map_err(|error| error.to_string())?;
            }
        }
        Ok(None)
    }

    fn matches_date(&self, date: Date) -> bool {
        let day = self.day & (1 << date.day()) != 0;
        let weekday = self.weekday & (1 << date.weekday().to_sunday_zero_offset()) != 0;
        let day_matches = if self.day_and_weekday {
            day && weekday
        } else {
            day || weekday
        };
        self.month & (1 << date.month()) != 0 && day_matches
    }
}

/// Resolve the optional type modifier before writing a column declaration.
pub fn column_timezone(sql_type: &crate::SqlTypeName) -> Result<&str, String> {
    match sql_type.modifiers.as_slice() {
        [] => Ok("UTC"),
        [crate::TypeModifier::StringLiteral(name)] => {
            timezone_from_name(name)?;
            Ok(name)
        }
        _ => Err("CRON accepts one optional IANA time zone string".into()),
    }
}

fn timezone_from_name(name: &str) -> Result<TimeZone, String> {
    if name.len() > MAX_ZONE_LEN
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'/' | b'+' | b'-'))
    {
        return Err("invalid IANA time zone name".into());
    }
    static TZDB: std::sync::LazyLock<jiff::tz::TimeZoneDatabase> =
        std::sync::LazyLock::new(jiff::tz::TimeZoneDatabase::bundled);
    TZDB.get(name).map_err(|error| error.to_string())
}

/// SQL constructor shared by value evaluation and literal folding.
pub fn construct(args: &[crate::Value]) -> Result<crate::Value, String> {
    use crate::Value;
    let schedule = match args {
        [Value::Text(expression)] => CronSchedule::parse(expression, "UTC")?,
        [Value::Text(expression), Value::Text(zone)] => CronSchedule::new(expression, zone)?,
        [Value::Null] | [Value::Null, _] | [_, Value::Null] => return Ok(Value::Null),
        _ => return Err("CRON expects an expression and an optional IANA time zone".into()),
    };
    Ok(Value::Cron(std::sync::Arc::new(schedule)))
}

const MAX_DAYS_BY_MONTH: [u8; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const MONTHS: &[&str] = &[
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: &[&str] = &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

fn parse_field(field: &str, min: u8, max: u8, names: &[&str]) -> Result<u64, String> {
    let invalid = || format!("invalid cron field '{field}' (expected {min} through {max})");
    let number = |raw: &str| {
        raw.parse::<u8>()
            .ok()
            .or_else(|| {
                names
                    .iter()
                    .position(|name| raw.eq_ignore_ascii_case(name))
                    .map(|index| min + index as u8)
            })
            .filter(|value| (min..=max).contains(value))
            .ok_or_else(invalid)
    };
    let mut mask = 0;
    for part in field.split(',') {
        let (base, step) = match part.split_once('/') {
            Some((base, step)) => (
                base,
                Some(
                    step.parse::<u8>()
                        .ok()
                        .filter(|step| *step > 0)
                        .ok_or_else(invalid)?,
                ),
            ),
            None => (part, None),
        };
        let (begin, end) = if base == "*" {
            (min, max)
        } else if let Some((begin, end)) = base.split_once('-') {
            (number(begin)?, number(end)?)
        } else {
            let begin = number(base)?;
            (begin, step.map_or(begin, |_| max))
        };
        if end < begin {
            return Err(invalid());
        }
        for value in (begin..=end).step_by(usize::from(step.unwrap_or(1))) {
            mask |= 1 << value;
        }
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(s: &str) -> i64 {
        s.parse::<Timestamp>()
            .expect("test timestamp")
            .as_millisecond()
    }

    #[test]
    fn uses_inclusive_start_and_exclusive_end() {
        let rule = CronSchedule::new("*/15 * * * *", "UTC").unwrap();
        let start = timestamp("2026-11-27T12:00:00Z");
        assert_eq!(rule.next(start).unwrap(), Some(start));
        assert_eq!(rule.next(start + 1).unwrap(), Some(start + 15 * MINUTE_MS));
        assert!(!rule.overlaps(start - 1, start).unwrap());
        assert!(rule.overlaps(start, start + 1).unwrap());
        assert!(!rule.overlaps(start, start).unwrap());
        assert!(rule.overlaps(start + 1, start).is_err());
    }

    #[test]
    fn skips_gaps_and_finds_both_sides_of_folds_in_instant_order() {
        let rule = CronSchedule::new("0,30 1-2 * * *", "America/New_York").unwrap();
        let before_fold = timestamp("2026-11-01T05:00:00.001Z");
        assert_eq!(
            rule.next(before_fold).unwrap(),
            Some(timestamp("2026-11-01T05:30:00Z"))
        );
        let during_fold = timestamp("2026-11-01T05:30:00.001Z");
        assert_eq!(
            rule.next(during_fold).unwrap(),
            Some(timestamp("2026-11-01T06:00:00Z"))
        );
        let before_gap = timestamp("2026-03-08T06:30:00.001Z");
        assert_eq!(
            rule.next(before_gap).unwrap(),
            Some(timestamp("2026-03-09T05:00:00Z"))
        );
        let midnight = timestamp("2026-03-08T05:00:00Z");
        let next_midnight = timestamp("2026-03-09T04:00:00Z");
        let instants = rule
            .occurrences(midnight, next_midnight)
            .expect("spring day")
            .collect::<Result<Vec<_>, _>>()
            .expect("valid window");
        assert_eq!(
            instants,
            [
                timestamp("2026-03-08T06:00:00Z"),
                timestamp("2026-03-08T06:30:00Z")
            ]
        );
    }

    #[test]
    fn supports_calendar_constraints_and_rejects_malformed_input() {
        let rule = CronSchedule::new("0 0 29 FEB *", "UTC").unwrap();
        let after_leap_day = timestamp("2096-03-01T00:00:00Z");
        assert_eq!(
            rule.next(after_leap_day).unwrap(),
            Some(timestamp("2104-02-29T00:00:00Z"))
        );
        let rule = CronSchedule::new("0 0 31 FEB MON", "UTC").unwrap();
        assert_eq!(
            rule.next(timestamp("2026-02-01T00:00:00Z")).unwrap(),
            Some(timestamp("2026-02-02T00:00:00Z"))
        );
        let impossible = CronSchedule::new("0 0 31 FEB *", "UTC").unwrap();
        assert_eq!(impossible.next(after_leap_day).unwrap(), None);
        assert!(CronSchedule::new("*/0 * * * *", "UTC").is_err());
        assert!(CronSchedule::new("* * * * *", "America/Missing").is_err());
    }
}

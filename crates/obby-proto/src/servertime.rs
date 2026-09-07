//! The `server-time` tag.
//!
//! The tag carries an ISO 8601 instant in UTC, such as `2026-09-06T10:00:00.000Z`. Turning it into
//! milliseconds is the whole job, and a date library is far more than that costs: everything here
//! has to build for wasm and for a target with no `std`, and a calendar crate is a large dependency
//! to carry for one conversion.

use alloc::string::String;
use core::fmt::Write as _;

/// Read a `server-time` value as milliseconds since the Unix epoch.
///
/// Returns `None` for anything that is not a well-formed UTC instant, and for any instant before
/// 1970, which no `server-time` legitimately carries. The caller should then stamp the message with
/// its own clock: a message with a nonsense timestamp sorts into the wrong place forever, which is
/// worse than one stamped a few milliseconds late.
pub fn parse(value: &str) -> Option<u64> {
    let (date, rest) = value.split_once('T')?;
    let time = rest.strip_suffix('Z')?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    // the calendar arithmetic below overflows on an absurd year, and ISO 8601 gives four digits
    // anyway, so anything outside that is not a timestamp we should be doing sums on
    if date_parts.next().is_some()
        || !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }

    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock_parts = clock.split(':');
    let hour: u64 = clock_parts.next()?.parse().ok()?;
    let minute: u64 = clock_parts.next()?.parse().ok()?;
    let second: u64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // a fraction is however many digits the server felt like sending, so scale it to milliseconds
    // rather than assuming three
    let millis = millis_from_fraction(fraction)?;

    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::try_from(hour * 3600 + minute * 60 + second).ok()?)?;
    u64::try_from(seconds.checked_mul(1000)?.checked_add(i64::from(millis))?).ok()
}

/// Write milliseconds since the Unix epoch as a `server-time` value.
///
/// Always three fractional digits and always UTC, which is the form the tag is defined in and the
/// one [`parse`] reads back exactly.
pub fn format(unix_ms: u64) -> String {
    let (seconds, millis) = (unix_ms / 1000, unix_ms % 1000);
    let (days, second_of_day) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    let mut out = String::new();
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        second_of_day / 3600,
        (second_of_day / 60) % 60,
        second_of_day % 60,
    );
    out
}

fn millis_from_fraction(fraction: &str) -> Option<u16> {
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = fraction.get(..3).unwrap_or(fraction);
    let value: u16 = digits.parse().ok()?;
    Some(match digits.len() {
        0 => 0,
        1 => value * 100,
        2 => value * 10,
        _ => value,
    })
}

/// Days from the Unix epoch to a calendar date, by Howard Hinnant's `days_from_civil`.
///
/// It is correct for every proleptic Gregorian date rather than only for a window of years, which a
/// simpler leap-year loop would not be.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A calendar date from days since the Unix epoch, the inverse of [`days_from_civil`].
///
/// Howard Hinnant's `civil_from_days`, restricted to dates at or after the epoch, which is all a
/// `u64` of milliseconds can express.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let era_days = days + 719_468;
    let era = era_days / 146_097;
    let day_of_era = era_days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_epoch() {
        assert_eq!(parse("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn reads_a_known_instant() {
        assert_eq!(parse("2026-09-06T10:00:00.000Z"), Some(1_788_688_800_000));
    }

    #[test]
    fn keeps_the_milliseconds() {
        let base = parse("2026-09-06T10:00:00.000Z").expect("valid");
        assert_eq!(parse("2026-09-06T10:00:00.123Z"), Some(base + 123));
    }

    #[test]
    fn scales_a_fraction_that_is_not_three_digits() {
        let base = parse("2026-09-06T10:00:00.000Z").expect("valid");
        assert_eq!(parse("2026-09-06T10:00:00.5Z"), Some(base + 500));
        assert_eq!(parse("2026-09-06T10:00:00.05Z"), Some(base + 50));
        assert_eq!(parse("2026-09-06T10:00:00.123456Z"), Some(base + 123));
    }

    #[test]
    fn a_missing_fraction_is_allowed() {
        assert_eq!(
            parse("2026-09-06T10:00:00Z"),
            parse("2026-09-06T10:00:00.000Z")
        );
    }

    #[test]
    fn handles_a_leap_day() {
        let feb = parse("2024-02-29T00:00:00.000Z").expect("2024 is a leap year");
        let mar = parse("2024-03-01T00:00:00.000Z").expect("valid");
        assert_eq!(mar - feb, 86_400_000);
    }

    #[test]
    fn handles_the_four_hundred_year_rule() {
        // 1900 was not a leap year and 2000 was, which a naive divisible-by-four test gets wrong.
        // These run against the calendar directly because 1900 predates the epoch that `parse`
        // returns from.
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1,
            "1900 is divisible by 100 and not by 400, so it has no 29th"
        );
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
    }

    #[test]
    fn refuses_a_year_the_calendar_arithmetic_cannot_hold() {
        // a hostile server sending this must not take the process down
        assert_eq!(parse("99999999999999999-01-01T00:00:00.000Z"), None);
        assert_eq!(parse("-4713-01-01T00:00:00.000Z"), None);
        assert_eq!(parse("10000-01-01T00:00:00.000Z"), None);
    }

    #[test]
    fn refuses_an_instant_before_the_epoch() {
        assert_eq!(parse("1969-12-31T23:59:59.000Z"), None);
    }

    #[test]
    fn accepts_a_leap_second() {
        assert!(parse("2016-12-31T23:59:60.000Z").is_some());
    }

    #[test]
    fn days_advance_by_exactly_one_day() {
        let a = parse("2026-09-06T00:00:00.000Z").expect("valid");
        let b = parse("2026-09-07T00:00:00.000Z").expect("valid");
        assert_eq!(b - a, 86_400_000);
    }

    #[test]
    fn writes_the_epoch() {
        assert_eq!(format(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn writes_a_known_instant() {
        assert_eq!(format(1_788_688_800_123), "2026-09-06T10:00:00.123Z");
    }

    #[test]
    fn pads_every_field() {
        assert_eq!(format(1_041_816_065_007), "2003-01-06T01:21:05.007Z");
    }

    #[test]
    fn writes_a_leap_day() {
        assert_eq!(format(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn round_trips_through_parse() {
        for ms in [
            0,
            1,
            999,
            86_399_999,
            86_400_000,
            951_782_400_000,
            1_788_688_800_123,
            253_402_300_799_999,
        ] {
            let text = format(ms);
            assert_eq!(parse(&text), Some(ms), "{text} should read back as {ms}");
        }
    }

    #[test]
    fn refuses_anything_malformed() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("2026-09-06"), None, "a date with no time");
        assert_eq!(parse("2026-09-06T10:00:00.000"), None, "no zone marker");
        assert_eq!(
            parse("2026-09-06T10:00:00.000+01:00"),
            None,
            "only UTC is defined"
        );
        assert_eq!(
            parse("2026-13-06T10:00:00.000Z"),
            None,
            "month out of range"
        );
        assert_eq!(parse("2026-09-06T24:00:00.000Z"), None, "hour out of range");
        assert_eq!(parse("2026-09-06T10:00.000Z"), None, "no seconds");
        assert_eq!(parse("not-a-date"), None);
        assert_eq!(
            parse("2026-09-06T10:00:00.abcZ"),
            None,
            "a non-numeric fraction"
        );
    }
}

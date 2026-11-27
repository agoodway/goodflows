//! The two time formats nextest's JUnit uses: RFC 3339 timestamps
//! (`2026-11-27T11:01:11.625+00:00`) and decimal seconds (`time="0.005"`).

/// Microseconds since the Unix epoch of an RFC 3339 timestamp, honoring its
/// UTC offset. `None` for anything else.
pub fn parse_us(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let separators = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if separators.iter().any(|&(i, c)| b.get(i) != Some(&c))
        || !matches!(b.get(10), Some(b'T' | b't' | b' '))
    {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut i = 19;
    let mut micros = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let digits = b[i..].iter().take_while(|d| d.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        micros = fraction_us(&b[i..i + digits]);
        i += digits;
    }
    let offset_minutes = match b.get(i..)? {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let minutes = num(i + 1, 2)? * 60 + num(i + 4, 2)?;
            if *sign == b'-' {
                -minutes
            } else {
                minutes
            }
        }
        _ => return None,
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second
        - offset_minutes * 60;
    Some(seconds * 1_000_000 + micros)
}

/// An RFC 3339 UTC timestamp with millisecond precision.
pub fn format_us(us: i64) -> String {
    let seconds = us.div_euclid(1_000_000);
    let millis = us.rem_euclid(1_000_000) / 1_000;
    let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

/// Whole microseconds in a nonnegative decimal number of seconds, without
/// going through a float. Digits past the sixth decimal are dropped.
pub fn seconds_us(s: &str) -> Option<u64> {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    let digits = |part: &str| part.bytes().all(|d| d.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let micros = whole
        .checked_mul(1_000_000)?
        .checked_add(fraction_us(fraction.as_bytes()) as u64)?;
    (micros < (1 << 53)).then_some(micros)
}

fn fraction_us(digits: &[u8]) -> i64 {
    (0..6).fold(0, |n, i| {
        n * 10 + digits.get(i).map_or(0, |d| i64::from(d - b'0'))
    })
}

// Howard Hinnant's days_from_civil and civil_from_days.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_honor_their_offset() {
        // 2024-02-29T23:30:00Z, written in UTC, in UTC+05:30 and in UTC-01:00.
        let utc = parse_us("2024-02-29T23:30:00.250Z").unwrap();
        assert_eq!(utc, 1_709_249_400_250_000);
        assert_eq!(parse_us("2024-03-01T05:00:00.25+05:30"), Some(utc));
        assert_eq!(parse_us("2024-02-29T22:30:00.250-01:00"), Some(utc));
        assert_eq!(format_us(utc), "2024-02-29T23:30:00.250Z");
        assert_eq!(parse_us("2024-02-29 23:30:00"), None);
        assert_eq!(parse_us("2024-02-29T23:30:00.Z"), None);
    }

    #[test]
    fn seconds_are_exact_microseconds() {
        // 0.29 * 1e6 is 289999.99999999994 as a float.
        assert_eq!(seconds_us("0.29"), Some(290_000));
        assert_eq!(seconds_us("1.001"), Some(1_001_000));
        assert_eq!(seconds_us("12"), Some(12_000_000));
        assert_eq!(seconds_us("0.1234567"), Some(123_456));
        assert_eq!(seconds_us("-1.0"), None);
        assert_eq!(seconds_us(".5"), None);
    }
}

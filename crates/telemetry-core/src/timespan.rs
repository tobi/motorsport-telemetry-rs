//! Race-time durations stored as integer milliseconds.
//!
//! Storage is [`u32`] milliseconds, `0..=`[`TIMESPAN_MS_MAX`] (100 hours).
//! That is enough for a lap, a stint, or a 24 h clock, and still exact for
//! averaging. Display is racing style: `M:SS.FFF` under one hour,
//! `H:MM:SS.FFF` from one hour up.

/// Largest legal `timespan_ms` value: 100 hours.
pub const TIMESPAN_MS_MAX: u32 = 100 * 60 * 60 * 1000;

/// Canonical unit / format token. Values are integer milliseconds.
pub const TIMESPAN_MS: &str = "timespan_ms";

/// Format milliseconds as `M:SS.FFF` or, from 1 h, `H:MM:SS.FFF`.
///
/// Values above [`TIMESPAN_MS_MAX`] display as the max. There is no space
/// in the string (`1:50.332`, never `1:50.332 `).
pub fn format_timespan_ms(ms: u32) -> String {
    let ms = ms.min(TIMESPAN_MS_MAX);
    let hours = ms / 3_600_000;
    let rem = ms % 3_600_000;
    let minutes = rem / 60_000;
    let rem = rem % 60_000;
    let seconds = rem / 1_000;
    let millis = rem % 1_000;
    if hours == 0 {
        format!("{minutes}:{seconds:02}.{millis:03}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
    }
}

/// Parse a racing time or a millisecond integer string.
///
/// Accepts `M:SS`, `M:SS.F`…`M:SS.FFF`, `H:MM:SS`, `H:MM:SS.F`…`H:MM:SS.FFF`.
/// Fractional digits pad to the right (`1:52.1` = `1:52.100`). Minutes in
/// `M:SS` may exceed 59 (a 90 minute session is `90:00.000`). When hours
/// are present, minutes and seconds are `00..=59`. Returns [`None`] when the
/// text is not a time or the value exceeds 100 h.
pub fn parse_timespan_ms(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let (hms, frac) = match text.split_once('.') {
        Some((left, right)) => {
            if right.is_empty() || right.len() > 3 || !right.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let millis = match right.len() {
                1 => right.parse::<u32>().ok()? * 100,
                2 => right.parse::<u32>().ok()? * 10,
                3 => right.parse::<u32>().ok()?,
                _ => return None,
            };
            (left, millis)
        }
        None => (text, 0),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let ms = match parts.as_slice() {
        [minutes, seconds] => {
            let minutes: u32 = minutes.parse().ok()?;
            let seconds: u32 = parse_two_digit(seconds)?;
            if seconds > 59 {
                return None;
            }
            minutes
                .checked_mul(60_000)?
                .checked_add(seconds * 1_000)?
                .checked_add(frac)?
        }
        [hours, minutes, seconds] => {
            let hours: u32 = hours.parse().ok()?;
            let minutes: u32 = parse_two_digit(minutes)?;
            let seconds: u32 = parse_two_digit(seconds)?;
            if minutes > 59 || seconds > 59 {
                return None;
            }
            hours
                .checked_mul(3_600_000)?
                .checked_add(minutes * 60_000)?
                .checked_add(seconds * 1_000)?
                .checked_add(frac)?
        }
        _ => return None,
    };
    (ms <= TIMESPAN_MS_MAX).then_some(ms)
}

fn parse_two_digit(text: &str) -> Option<u32> {
    if text.len() != 2 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Mean of millisecond timespans, rounded to the nearest millisecond.
pub fn average_timespan_ms(values: &[u32]) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    let n = values.len() as u64;
    let sum: u64 = values.iter().map(|&ms| u64::from(ms)).sum();
    Some(((sum + n / 2) / n) as u32)
}

/// True when `ms` fits in the 100 h store.
pub fn timespan_ms_in_range(ms: u64) -> bool {
    ms <= u64::from(TIMESPAN_MS_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_lap_and_stint() {
        assert_eq!(format_timespan_ms(0), "0:00.000");
        assert_eq!(format_timespan_ms(110_332), "1:50.332");
        assert_eq!(format_timespan_ms(32_104), "0:32.104");
        assert_eq!(format_timespan_ms(5_400_000), "1:30:00.000");
        assert_eq!(format_timespan_ms(TIMESPAN_MS_MAX), "100:00:00.000");
    }

    #[test]
    fn parses_racing_strings() {
        assert_eq!(parse_timespan_ms("1:50.332"), Some(110_332));
        assert_eq!(parse_timespan_ms("1:52.1"), Some(112_100));
        assert_eq!(parse_timespan_ms("1:30:00"), Some(5_400_000));
        assert_eq!(parse_timespan_ms("90:00.000"), Some(5_400_000));
        assert_eq!(parse_timespan_ms("100:00:00.000"), Some(TIMESPAN_MS_MAX));
        assert_eq!(parse_timespan_ms("0:00.000"), Some(0));
    }

    #[test]
    fn rejects_out_of_range_and_junk() {
        assert_eq!(parse_timespan_ms("100:00:00.001"), None);
        assert_eq!(parse_timespan_ms("101:00:00.000"), None);
        assert_eq!(parse_timespan_ms("IMSA"), None);
        assert_eq!(parse_timespan_ms("28"), None);
        assert_eq!(parse_timespan_ms("1:60.000"), None);
        assert_eq!(parse_timespan_ms("1:50.3320"), None);
        assert_eq!(parse_timespan_ms("1: 50.332"), None);
    }

    #[test]
    fn average_is_nearest_ms() {
        assert_eq!(average_timespan_ms(&[110_332, 112_104]), Some(111_218));
        assert_eq!(average_timespan_ms(&[]), None);
    }

    #[test]
    fn u32_holds_100_hours() {
        assert!(u32::try_from(u64::from(TIMESPAN_MS_MAX)).is_ok());
        assert!(TIMESPAN_MS_MAX < u32::MAX);
    }
}

//! GTFS times of day. A time is seconds after "noon minus 12 hours" of the
//! service date, so it can exceed 24:00:00 for trips that run past midnight.

/// Seconds since the start of a service day. Signed so that offsets between
/// service days can be applied without conversions.
pub type ServiceSeconds = i32;

/// Largest time accepted: 7 days. Anything later is treated as malformed.
pub const MAX_SERVICE_SECONDS: ServiceSeconds = 7 * 24 * 3600;

/// Parse `H:MM:SS` or `HH:MM:SS` (hours may exceed 23). Leading and trailing
/// spaces are tolerated because some feeds pad single-digit hours with one.
pub fn parse_time(s: &str) -> Option<ServiceSeconds> {
    let s = s.trim();
    let mut parts = s.split(':');
    let h = parts.next()?;
    let m = parts.next()?;
    let sec = parts.next()?;
    if parts.next().is_some() || h.is_empty() || h.len() > 3 || m.len() != 2 || sec.len() != 2 {
        return None;
    }
    let digits = |p: &str| -> Option<i32> {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse().ok()
        } else {
            None
        }
    };
    let (h, m, sec) = (digits(h)?, digits(m)?, digits(sec)?);
    if m > 59 || sec > 59 {
        return None;
    }
    let total = h * 3600 + m * 60 + sec;
    (total <= MAX_SERVICE_SECONDS).then_some(total)
}

/// Format as `HH:MM:SS`, keeping hours past 24 and a sign for negatives.
pub fn format_time(t: ServiceSeconds) -> String {
    let sign = if t < 0 { "-" } else { "" };
    let t = t.unsigned_abs();
    format!("{sign}{:02}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_ordinary_and_late_times() {
        assert_eq!(parse_time("04:30:00"), Some(4 * 3600 + 30 * 60));
        assert_eq!(parse_time("4:30:00"), Some(4 * 3600 + 30 * 60));
        assert_eq!(parse_time(" 4:30:00"), Some(4 * 3600 + 30 * 60));
        assert_eq!(parse_time("25:10:05"), Some(25 * 3600 + 10 * 60 + 5));
        assert_eq!(parse_time("00:00:00"), Some(0));
    }

    #[test]
    fn rejects_malformed_and_negative_times() {
        for bad in [
            "",
            "12:00",
            "12:60:00",
            "12:00:60",
            "-1:00:00",
            "ab:cd:ef",
            "12:0:00",
            "1:00:00:00",
            "12:00:0x",
            "999:00:00",
            "+1:00:00",
        ] {
            assert_eq!(parse_time(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn formats_past_midnight_and_negative() {
        assert_eq!(format_time(25 * 3600 + 61), "25:01:01");
        assert_eq!(format_time(-90), "-00:01:30");
    }

    proptest! {
        #[test]
        fn format_then_parse_roundtrips(t in 0..MAX_SERVICE_SECONDS) {
            prop_assert_eq!(parse_time(&format_time(t)), Some(t));
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,12}") {
            let _ = parse_time(&s);
        }
    }
}

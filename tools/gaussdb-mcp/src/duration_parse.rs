use std::time::Duration;

/// Parse a human-friendly duration string into a `Duration`.
///
/// Accepted formats:
/// - Bare integer: interpreted as seconds (e.g. `"30"` → 30s)
/// - With unit suffix: `"500ms"`, `"30s"`, `"5m"`/`"5min"`, `"1h"`/`"1hr"`, `"2d"`
/// - Case-insensitive, whitespace-trimmed
/// - Returns `Err` with a descriptive message for invalid input
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();

    if s.is_empty() {
        return Err("empty string: expected a duration like \"30\" or \"500ms\"".to_string());
    }

    // Find the first alphabetic character to split number from unit.
    let alpha_pos = s.find(|c: char| c.is_ascii_alphabetic());

    let (num_str, unit_str) = match alpha_pos {
        Some(pos) => (&s[..pos], &s[pos..]),
        None => (s, ""),
    };

    // Reject entirely alphabetic strings (no numeric part).
    if num_str.is_empty() {
        return Err(format!(
            "invalid duration '{}': expected a number followed by an optional unit (ms/s/m/h/d)",
            s
        ));
    }

    let value: u64 = num_str
        .parse()
        .map_err(|_| format!("invalid number '{}' in duration '{}'", num_str, s))?;

    let unit = unit_str.trim().to_lowercase();
    let millis = match unit.as_str() {
        "" => value.checked_mul(1000), // bare integer → seconds
        "ms" => Some(value),
        "s" => value.checked_mul(1000),
        "m" | "min" | "minutes" => value.checked_mul(60_000),
        "h" | "hr" => value.checked_mul(3_600_000),
        "d" => value.checked_mul(86_400_000),
        _ => {
            return Err(format!(
                "unknown unit '{}' in duration '{}': expected ms/s/m/min/h/hr/d",
                unit_str, s
            ));
        }
    };

    match millis {
        Some(ms) => Ok(Duration::from_millis(ms)),
        None => Err(format!(
            "value '{}' in duration '{}' overflows",
            num_str, s
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Success cases ────────────────────────────────────────────────

    #[test]
    fn test_parse_bare_integer_seconds() {
        let d = parse_duration("30").unwrap();
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn test_parse_seconds_suffix() {
        let d = parse_duration("45s").unwrap();
        assert_eq!(d, Duration::from_secs(45));
    }

    #[test]
    fn test_parse_milliseconds_suffix() {
        let d = parse_duration("500ms").unwrap();
        assert_eq!(d, Duration::from_millis(500));
    }

    #[test]
    fn test_parse_minutes_suffix() {
        // m
        let d = parse_duration("2m").unwrap();
        assert_eq!(d, Duration::from_secs(120));
        // min
        let d = parse_duration("3min").unwrap();
        assert_eq!(d, Duration::from_secs(180));
        // minutes
        let d = parse_duration("1minutes").unwrap();
        assert_eq!(d, Duration::from_secs(60));
    }

    #[test]
    fn test_parse_hours_suffix() {
        // h
        let d = parse_duration("2h").unwrap();
        assert_eq!(d, Duration::from_secs(7200));
        // hr
        let d = parse_duration("1hr").unwrap();
        assert_eq!(d, Duration::from_secs(3600));
    }

    #[test]
    fn test_parse_days_suffix() {
        let d = parse_duration("2d").unwrap();
        assert_eq!(d, Duration::from_secs(172800));
    }

    #[test]
    fn test_parse_case_insensitive_and_trimmed() {
        // Case insensitivity
        let d = parse_duration("  5S  ").unwrap();
        assert_eq!(d, Duration::from_secs(5));
        let d = parse_duration("  1H  ").unwrap();
        assert_eq!(d, Duration::from_secs(3600));
        let d = parse_duration(" 100MS ").unwrap();
        assert_eq!(d, Duration::from_millis(100));
        let d = parse_duration("  2MIN  ").unwrap();
        assert_eq!(d, Duration::from_secs(120));
        // Trimmed
        let d = parse_duration("  10  ").unwrap();
        assert_eq!(d, Duration::from_secs(10));
    }

    // ── Error cases ──────────────────────────────────────────────────

    #[test]
    fn test_parse_empty_string_error() {
        let err = parse_duration("").unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }

    #[test]
    fn test_parse_invalid_number_error() {
        let err = parse_duration("abc").unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }

    #[test]
    fn test_parse_unknown_unit_error() {
        let err = parse_duration("10x").unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
        let err = parse_duration("5weeks").unwrap_err();
        assert!(!err.is_empty(), "error message should not be empty");
    }
}

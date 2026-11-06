//! Sizes and times written for a person to read: the same on the command
//! line, in the dashboard, in the report and in the log.

/// A size in binary units: `512 B`, `8.0 MiB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// `time` in UTC as RFC 3339, to the second: `2026-11-06T03:17:00Z`. The
/// report's `started_at` and `finished_at`.
pub fn rfc3339_utc(time: std::time::SystemTime) -> String {
    let (date, _) = utc_parts(time);
    format!("{date}Z")
}

/// `time` in UTC as RFC 3339, to the millisecond:
/// `2026-11-06T03:17:00.123Z`. What each log line is stamped with.
pub fn rfc3339_utc_millis(time: std::time::SystemTime) -> String {
    let (date, millis) = utc_parts(time);
    format!("{date}.{millis:03}Z")
}

/// The date and time to the second, without a zone, and the milliseconds
/// past it. A time before 1970 counts as 1970.
fn utc_parts(time: std::time::SystemTime) -> (String, u32) {
    let elapsed = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = elapsed.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    // Days since 1970-01-01 to a civil date, by the well-known
    // civil-from-days algorithm; every value here is non-negative.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"),
        elapsed.subsec_millis(),
    )
}

#[cfg(test)]
mod tests {
    use super::{format_size, rfc3339_utc, rfc3339_utc_millis};

    #[test]
    fn sizes_are_in_binary_units_with_one_decimal() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(8 * 1024 * 1024), "8.0 MiB");
        assert_eq!(format_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn times_are_utc_rfc3339() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = UNIX_EPOCH + Duration::from_millis(1_794_000_612_484);
        assert_eq!(rfc3339_utc(at), "2026-11-06T21:30:12Z");
        assert_eq!(rfc3339_utc_millis(at), "2026-11-06T21:30:12.484Z");
        assert_eq!(rfc3339_utc_millis(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }
}

//! RFC 3339 timestamps from Unix milliseconds, with no clock of their own.

/// Formats Unix milliseconds as RFC 3339 UTC with milliseconds, for example
/// `2026-11-13T05:30:00.000Z` (the form wallet sign-in messages carry).
///
/// Years past 9999 are outside RFC 3339; the caller's clock never reaches
/// them, and they are written with more digits rather than wrapped.
#[must_use]
pub fn rfc3339_ms(unix_ms: u64) -> String {
    let ms = unix_ms % 1000;
    let secs = unix_ms / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day)
/// (H. Hinnant's `civil_from_days`, for non-negative day counts).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339_ms(1_763_011_800_123), "2025-11-13T05:30:00.123Z");
        assert_eq!(rfc3339_ms(1_794_547_800_000), "2026-11-13T05:30:00.000Z");
        assert_eq!(rfc3339_ms(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
    }
}

//! Number and date forms the contracts print.

/// `3214` as `3,214`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The UTC calendar date `(year, month, day)` of a Unix time in milliseconds.
pub fn utc_date(ms: u64) -> (i64, u32, u32) {
    // Days to civil date (proleptic Gregorian), after H. Hinnant.
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `YYYY-MM-DD` (UTC) of a Unix time in milliseconds.
pub fn iso_date(ms: u64) -> String {
    let (y, m, d) = utc_date(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(987), "987");
        assert_eq!(thousands(3214), "3,214");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(thousands(u64::from(u32::MAX)), "4,294,967,295");
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(951_782_400_000), "2000-02-29");
        assert_eq!(iso_date(1_763_596_800_000), "2025-11-20");
        assert_eq!(iso_date(1_795_132_799_999), "2026-11-19");
    }
}

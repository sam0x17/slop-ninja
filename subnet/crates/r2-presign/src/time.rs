//! UTC calendar fields of a Unix time, for SigV4's `yyyymmdd` and
//! `yyyymmddThhmmssZ`, without a time library.

use crate::buf::Buf;
use core::fmt::Write;

/// `(year, month, day)` of the day `days` after 1970-01-01 (proleptic
/// Gregorian; Howard Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// SigV4 date stamps of `unix_secs`: `(yyyymmdd, yyyymmddThhmmssZ)`. Years
/// outside 0..=9999 have no four-digit form and are refused.
pub fn amz_dates(unix_secs: u64) -> Option<(Buf<8>, Buf<16>)> {
    let days = i64::try_from(unix_secs / 86_400).ok()?;
    let rem = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    if !(0..=9999).contains(&y) {
        return None;
    }
    let (hh, mm, ss) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let mut date = Buf::new();
    write!(date, "{y:04}{m:02}{d:02}").ok()?;
    let mut stamp = Buf::new();
    write!(stamp, "{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z").ok()?;
    Some((date, stamp))
}

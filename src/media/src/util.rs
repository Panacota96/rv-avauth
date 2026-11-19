//! Small helpers: bounded integer reads from slices, text clean-up, fourcc display and date arithmetic
//! (plain integer arithmetic, no date library: the ISO BMFF epoch is 1904-01-01 UTC).

use crate::model::limits;

pub fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
pub fn be24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(((s[0] as u32) << 16) | ((s[1] as u32) << 8) | s[2] as u32)
}
pub fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
pub fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
pub fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
pub fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
pub fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Printable fourcc (`©too` shown with the copyright sign), else hex.
pub fn fourcc(b: &[u8]) -> String {
    if b.len() == 4
        && b.iter()
            .enumerate()
            .all(|(i, &c)| (0x20..0x7f).contains(&c) || (i == 0 && c == 0xA9))
    {
        b.iter()
            .map(|&c| if c == 0xA9 { '©' } else { c as char })
            .collect()
    } else {
        b.iter().map(|c| format!("{:02x}", c)).collect()
    }
}

/// True when the 4 bytes look like a box or chunk type (printable ASCII, or © first).
pub fn plausible_fourcc(b: &[u8]) -> bool {
    b.len() == 4
        && b.iter()
            .enumerate()
            .all(|(i, &c)| (0x20..0x7f).contains(&c) || (i == 0 && c == 0xA9))
}

/// Decodes Latin-1 bytes.
pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// UTF-8 if valid, else Latin-1 (old tools write Latin-1 into "UTF-8" fields).
pub fn text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => latin1(b),
    }
}

/// Drops NULs and control characters, trims, caps the length (characters).
pub fn clean_text(s: &str) -> String {
    let t: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(limits::MAX_TAG_VALUE)
        .collect();
    t.trim().to_string()
}

/// Seconds between 1904-01-01 and 1970-01-01.
pub const MAC_EPOCH_OFFSET: i64 = 2_082_844_800;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) from days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// RFC 3339 UTC string ("2026-03-01T10:00:00Z") for years 1900–9999, else None.
pub fn rfc3339(unix: i64) -> Option<String> {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    if !(1900..=9999).contains(&y) {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    ))
}

/// ISO BMFF time (seconds since 1904-01-01 UTC) → Unix seconds; None for 0 (not set).
pub fn mac_to_unix(v: u64) -> Option<i64> {
    if v == 0 || v > i64::MAX as u64 {
        return None;
    }
    Some(v as i64 - MAC_EPOCH_OFFSET)
}

fn digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses the date texts found in tags: "2026-03-01", "2026-03-01T10:00:00", with "Z", "+01:00" or
/// "+0100", "2026:03:01 10:00:00", "2026-03-01 10:00:00". Returns (unix seconds, has a time zone).
/// A text without a time zone is read as if it were UTC (the caller applies a ±26 h tolerance).
pub fn parse_tag_date(s: &str) -> Option<(i64, bool)> {
    let s = s.trim();
    if s.len() < 10 {
        return None;
    }
    let b = s.as_bytes();
    let sep = b[4];
    if !(sep == b'-' || sep == b':') || b[7] != sep {
        return None;
    }
    let y = digits(s.get(0..4)?)?;
    let m = digits(s.get(5..7)?)? as u32;
    let d = digits(s.get(8..10)?)? as u32;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut unix = days_from_civil(y, m, d) * 86_400;
    let rest = s.get(10..)?;
    let mut zoned = false;
    if rest.len() >= 9 && matches!(rest.as_bytes()[0], b'T' | b' ') {
        let hh = digits(rest.get(1..3)?)?;
        let mm = digits(rest.get(4..6)?)?;
        let ss = digits(rest.get(7..9)?)?;
        if hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        unix += hh * 3600 + mm * 60 + ss;
        // Skip fractional seconds.
        let mut tz = rest.get(9..)?;
        if let Some(t) = tz.strip_prefix('.') {
            let n = t.bytes().take_while(|b| b.is_ascii_digit()).count();
            tz = t.get(n..)?;
        }
        if tz == "Z" || tz == "z" {
            zoned = true;
        } else if tz.len() >= 5 && (tz.starts_with('+') || tz.starts_with('-')) {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let body: String = tz.get(1..)?.chars().filter(|c| *c != ':').collect();
            if body.len() == 4 {
                let oh = digits(body.get(0..2)?)?;
                let om = digits(body.get(2..4)?)?;
                unix -= sign * (oh * 3600 + om * 60);
                zoned = true;
            }
        }
    }
    Some((unix, zoned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_and_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1904, 1, 1) * 86_400, -MAC_EPOCH_OFFSET);
        for z in [-30_000i64, -1, 0, 1, 10_957, 20_000, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(rfc3339(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_772_359_200).unwrap(), "2026-03-01T10:00:00Z");
        assert_eq!(mac_to_unix(0), None);
        assert_eq!(
            mac_to_unix(1_772_359_200 + MAC_EPOCH_OFFSET as u64),
            Some(1_772_359_200)
        );
    }

    #[test]
    fn tag_dates() {
        assert_eq!(
            parse_tag_date("2026-03-01T10:00:00Z"),
            Some((1_772_359_200, true))
        );
        assert_eq!(
            parse_tag_date("2026-03-01T11:00:00+0100"),
            Some((1_772_359_200, true))
        );
        assert_eq!(
            parse_tag_date("2026-03-01T11:00:00.123+01:00"),
            Some((1_772_359_200, true))
        );
        assert_eq!(
            parse_tag_date("2026:03:01 10:00:00"),
            Some((1_772_359_200, false))
        );
        assert_eq!(parse_tag_date("2026-03-01"), Some((1_772_323_200, false)));
        assert_eq!(parse_tag_date("2026"), None);
        assert_eq!(parse_tag_date("2026-13-01"), None);
        assert_eq!(parse_tag_date("abcd-ef-gh"), None);
        assert_eq!(parse_tag_date("2026-03-01T1x:00:00"), None);
    }

    #[test]
    fn text_helpers() {
        assert_eq!(fourcc(b"moov"), "moov");
        assert_eq!(fourcc(&[0xA9, b't', b'o', b'o']), "©too");
        assert_eq!(fourcc(&[0, 1, 2, 3]), "00010203");
        assert_eq!(clean_text(" a\0b\n "), "ab");
        assert_eq!(text(&[0xE9]), "é");
    }
}

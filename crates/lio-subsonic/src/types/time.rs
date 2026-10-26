//! Minimal RFC 3339 parsing for Subsonic timestamps (no date library needed).

/// Parses `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)` into Unix seconds.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        rest = &frac[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        tz if tz.len() == 6 && matches!(&tz[..1], "+" | "-") => {
            let h: i64 = tz[1..3].parse().ok()?;
            let m: i64 = tz[4..6].parse().ok()?;
            let sign = if &tz[..1] == "-" { -1 } else { 1 };
            sign * (h * 3600 + m * 60)
        }
        _ => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec - offset)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_navidrome_timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_timestamp("2026-10-25T15:04:19.832568439Z"),
            Some(1_792_940_659)
        );
        assert_eq!(
            parse_timestamp("2026-10-25T17:04:19+02:00"),
            Some(1_792_940_659)
        );
        assert_eq!(parse_timestamp("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(parse_timestamp("garbage"), None);
        assert_eq!(parse_timestamp(""), None);
    }
}

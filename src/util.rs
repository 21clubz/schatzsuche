//! Small helpers with no home of their own.

use std::time::{SystemTime, UNIX_EPOCH};

/// The machine's hostname, or `"unknown"` if it cannot be read.
///
/// Windows has no `gethostname` in the `libc` crate, and exposes the name as an
/// environment variable instead.
#[cfg(unix)]
pub fn hostname() -> String {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: buf is 256 elements and the length passed matches it exactly.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return "unknown".to_string();
    }
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(windows)]
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_string())
}

/// True when this process runs from inside a macOS application bundle.
///
/// Two very different questions depend on this. `main` uses it to tell a
/// double-click from a shell invocation, because a desktop launch carries no
/// arguments to distinguish it. The desktop notifier uses it to decide which
/// bundle identifier to claim: only a bundled build has one that LaunchServices
/// can resolve.
///
/// The path is an exact signal rather than a guess. An earlier version of the
/// `main` check asked whether stdout was a terminal, which is wrong twice over:
/// `schatzsuche > log.txt` from a shell also has a non-terminal stdout, and on
/// a CI runner *nothing* is a terminal.
pub fn in_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .map(|d| d.ends_with("Contents/MacOS"))
        .unwrap_or(false)
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats a Unix timestamp as RFC 3339 in UTC.
///
/// Hand-rolled rather than pulling a date library in for one function; the
/// civil-date conversion is Howard Hinnant's well-known algorithm.
pub fn rfc3339(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs_of_day = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Formats a Unix timestamp the way a person writes a date: `30.07.2026,
/// 05:54 Uhr (UTC)`.
///
/// UTC und ausdrücklich so beschriftet. Die Ortszeit wäre freundlicher, aber
/// sie hängt an einer Zeitzonendatenbank, und die kostet eine Abhängigkeit im
/// Baum eines Programms, das Wallet-Schlüssel erzeugt. Eine Zeitangabe, die
/// dazuschreibt, welche Uhr sie meint, ist ehrlicher als eine, die es
/// verschweigt.
pub fn human_utc(unix: u64) -> String {
    let (y, m, d) = civil_from_days((unix / 86_400) as i64);
    let secs_of_day = unix % 86_400;
    format!(
        "{:02}.{:02}.{:04}, {:02}:{:02} Uhr (UTC)",
        d,
        m,
        y,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Formats a satoshi amount as BTC with 8 decimals.
pub fn format_btc(sats: u64) -> String {
    format!("{}.{:08} BTC", sats / 100_000_000, sats % 100_000_000)
}

/// Formats a duration as `NdNNh NNm NNs`, dropping empty leading units.
pub fn format_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h:02}h {m:02}m {s:02}s")
    } else if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Groups digits with thin spaces: 25000000 becomes "25 000 000".
pub fn group_digits(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut parts = Vec::new();
    while n > 0 {
        parts.push(format!("{:03}", n % 1000));
        n /= 1000;
    }
    parts.reverse();
    let mut s = parts.join(" ");
    while s.starts_with('0') && s.len() > 1 && !s.starts_with("0 ") {
        s.remove(0);
    }
    s
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which is where naive date maths usually breaks.
        assert_eq!(rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_735_689_599), "2024-12-31T23:59:59Z");
    }

    /// Dieselben Zeitpunkte wie oben, in der Schreibweise der Fundliste.
    #[test]
    fn human_dates_read_like_dates() {
        assert_eq!(human_utc(0), "01.01.1970, 00:00 Uhr (UTC)");
        assert_eq!(human_utc(1_000_000_000), "09.09.2001, 01:46 Uhr (UTC)");
        assert_eq!(human_utc(1_709_164_800), "29.02.2024, 00:00 Uhr (UTC)");
    }

    #[test]
    fn digit_grouping() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(25_000_000), "25 000 000");
        assert_eq!(group_digits(1_234_567), "1 234 567");
    }

    #[test]
    fn btc_formatting() {
        assert_eq!(format_btc(0), "0.00000000 BTC");
        assert_eq!(format_btc(100_000_000), "1.00000000 BTC");
        assert_eq!(format_btc(123_456_789), "1.23456789 BTC");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(3661), "1h 01m 01s");
        assert_eq!(format_duration(90_061), "1d 01h 01m 01s");
    }
}

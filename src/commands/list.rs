use std::io;

use crate::state;

/// `synapse list` — all packages tracked in state.json with versions and timestamps.
pub fn run() -> io::Result<()> {
    let cfg = state::config_dir();
    let st = state::read(&cfg)?;

    if st.packages.is_empty() {
        println!("No packages installed. Run `synapse install` to get started.");
        return Ok(());
    }

    println!("{:<16} {:<12} INSTALLED AT", "PACKAGE", "VERSION");
    println!("{}", "-".repeat(52));

    let mut pkgs: Vec<_> = st.packages.iter().collect();
    pkgs.sort_by_key(|(k, _)| *k);

    for (name, rec) in &pkgs {
        let ts = format_timestamp(rec.installed_at);
        println!("{:<16} {:<12} {}", name, rec.version, ts);
    }

    Ok(())
}

pub(crate) fn format_timestamp(secs: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let t = UNIX_EPOCH + Duration::from_secs(secs);
    // Simple ISO-8601 approximation without a time library dependency.
    // ponytail: swap for chrono/time if formatted timestamps matter; UNIX_EPOCH
    // offset arithmetic is enough for a readable list command.
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let total = d.as_secs();
            // Seconds since epoch → rough date via Julian day calculation.
            // Good for dates 1970-9999 (no dependency required).
            let days = total / 86400;
            let rem = total % 86400;
            let h = rem / 3600;
            let m = (rem % 3600) / 60;
            let s = rem % 60;

            // Algorithm from https://en.wikipedia.org/wiki/Julian_day#Julian_day_number_calculation
            let jd = days as i64 + 2440588; // Julian day for Unix epoch
            let l = jd + 68569;
            let n = (4 * l) / 146097;
            let l = l - (146097 * n + 3) / 4;
            let i = (4000 * (l + 1)) / 1461001;
            let l = l - (1461 * i) / 4 + 31;
            let j = (80 * l) / 2447;
            let day = l - (2447 * j) / 80;
            let l = j / 11;
            let month = j + 2 - 12 * l;
            let year = 100 * (n - 49) + i + l;

            format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02} UTC")
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_unix_epoch() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn format_known_date() {
        // 2026-08-01 00:00:00 UTC
        assert_eq!(format_timestamp(1785542400), "2026-08-01 00:00:00 UTC");
    }
}

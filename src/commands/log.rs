use std::io;
use std::io::{BufRead, Write as _};
use std::path::PathBuf;

use crate::state;

/// Path to the append-only install/update log.
pub fn log_path() -> PathBuf {
    state::config_dir().join("install.log")
}

/// Append one line to the log. Called by update and install flows.
pub fn append(entry: &str) -> io::Result<()> {
    use std::fs::OpenOptions;
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{entry}")
}

/// Render one raw log line for display.
///
/// Entries are stored as `<epoch> <rest>` so the file stays append-only and
/// machine-readable; only the presentation layer converts the timestamp. A line
/// whose first field is not an epoch is passed through untouched rather than
/// mangled.
fn humanize(line: &str) -> String {
    match line.split_once(' ') {
        Some((ts, rest)) => match ts.parse::<u64>() {
            Ok(secs) => format!("{}  {rest}", super::list::format_timestamp(secs)),
            Err(_) => line.to_string(),
        },
        None => line.to_string(),
    }
}

/// `synapse log` — print recent install and update history.
pub fn run() -> io::Result<()> {
    let path = log_path();
    match std::fs::File::open(&path) {
        Ok(f) => {
            let reader = io::BufReader::new(f);
            let mut count = 0usize;
            for line in reader.lines() {
                println!("{}", humanize(&line?));
                count += 1;
            }
            if count == 0 {
                println!("No history yet.");
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("No history yet.");
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `append` writes through `log_path()`, which is derived from
    /// `XDG_CONFIG_HOME`. Setting it lets us exercise the real code path
    /// instead of reimplementing the append in the test.
    ///
    /// Serialised via a mutex because env vars are process-global.
    fn with_scratch_config<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "synapse-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        let result = f(&dir);

        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        std::fs::remove_dir_all(&dir).ok();

        result
    }

    #[test]
    fn append_creates_log_and_preserves_order() {
        with_scratch_config(|_| {
            // append() must create the config dir itself — first run has none.
            append("1785542400 installed herdr 0.7.5").unwrap();
            append("1785546000 installed omp 17.2.2").unwrap();

            let content = std::fs::read_to_string(log_path()).unwrap();
            let lines: Vec<&str> = content.lines().collect();

            assert_eq!(lines.len(), 2, "expected exactly two entries");
            assert!(
                lines[0].contains("herdr"),
                "first entry not first: {lines:?}"
            );
            assert!(
                lines[1].contains("omp"),
                "append overwrote instead of appending"
            );
        });
    }

    #[test]
    fn append_is_additive_not_truncating() {
        with_scratch_config(|_| {
            for i in 0..5 {
                append(&format!("entry {i}")).unwrap();
            }
            let content = std::fs::read_to_string(log_path()).unwrap();
            assert_eq!(content.lines().count(), 5, "entries were lost");
        });
    }

    #[test]
    fn log_path_lands_under_config_dir() {
        with_scratch_config(|dir| {
            let p = log_path();
            assert!(p.starts_with(dir), "log escaped the config dir: {p:?}");
            assert_eq!(p.file_name().unwrap(), "install.log");
        });
    }

    #[test]
    fn humanize_formats_the_epoch_and_keeps_the_message() {
        let out = humanize("1785542400 installed herdr 0.7.5");
        assert_eq!(out, "2026-08-01 00:00:00 UTC  installed herdr 0.7.5");
    }

    #[test]
    fn humanize_passes_through_a_non_epoch_line() {
        // A hand-edited or future-format line must not be mangled or dropped.
        for line in ["not-a-timestamp installed herdr", "nofieldsatall", ""] {
            assert_eq!(humanize(line), line, "line was rewritten: {line:?}");
        }
    }

    #[test]
    fn stored_lines_stay_machine_readable() {
        // humanize is presentation-only: the file itself keeps raw epochs so it
        // remains greppable and parseable.
        with_scratch_config(|_| {
            append("1785542400 installed herdr 0.7.5").unwrap();
            let raw = std::fs::read_to_string(log_path()).unwrap();
            assert!(
                raw.starts_with("1785542400 "),
                "log file should store the raw epoch, got: {raw:?}"
            );
        });
    }
}

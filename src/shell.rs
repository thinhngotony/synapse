//! Shell integration: PATH setup, completions, and post-install next steps.
//!
//! Everything written into a user's rc file is wrapped in a marker block so it
//! can be found, replaced, or removed by hand:
//!
//! ```text
//! # >>> synapse >>>
//! ...managed lines...
//! # <<< synapse <<<
//! ```
//!
//! Re-running the installer replaces the block in place rather than appending a
//! second copy, so PATH never accumulates duplicates.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Opening marker of the managed block.
pub const MARKER_START: &str = "# >>> synapse >>>";
/// Closing marker of the managed block.
pub const MARKER_END: &str = "# <<< synapse <<<";

/// A shell Synapse knows how to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// Name used for completion generation and messages.
    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }

    /// The rc file this shell reads, relative to `$HOME`.
    pub fn rc_relative(self) -> &'static str {
        match self {
            Shell::Bash => ".bashrc",
            Shell::Zsh => ".zshrc",
            Shell::Fish => ".config/fish/config.fish",
        }
    }

    /// Absolute rc path for a given home directory.
    pub fn rc_path(self, home: &Path) -> PathBuf {
        home.join(self.rc_relative())
    }

    /// Lines that put the Nix profile on PATH, in this shell's syntax.
    ///
    /// bash/zsh share POSIX syntax; fish needs `fish_add_path`.
    pub fn path_snippet(self) -> String {
        match self {
            Shell::Bash | Shell::Zsh => concat!(
                "# Nix single-user profile\n",
                "if [ -e \"$HOME/.nix-profile/etc/profile.d/nix.sh\" ]; then\n",
                "  . \"$HOME/.nix-profile/etc/profile.d/nix.sh\"\n",
                "fi\n",
                "case \":$PATH:\" in\n",
                "  *\":$HOME/.nix-profile/bin:\"*) ;;\n",
                "  *) PATH=\"$HOME/.nix-profile/bin:$PATH\" ;;\n",
                "esac\n",
                "export PATH\n",
            )
            .to_string(),
            Shell::Fish => concat!(
                "# Nix single-user profile\n",
                "if test -d \"$HOME/.nix-profile/bin\"\n",
                "    fish_add_path --path --prepend \"$HOME/.nix-profile/bin\"\n",
                "end\n",
            )
            .to_string(),
        }
    }
}

/// Every shell Synapse can configure.
pub const ALL_SHELLS: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

/// Detect which shells are actually present for this user.
///
/// A shell counts as present if its rc file exists, or if it is the user's
/// `$SHELL`. We never create an rc file for a shell the user does not use.
pub fn detect_shells(home: &Path, shell_env: Option<&str>) -> Vec<Shell> {
    let current = shell_env.and_then(|s| {
        let base = s.rsplit('/').next().unwrap_or(s);
        ALL_SHELLS.iter().copied().find(|sh| sh.name() == base)
    });

    let mut found: Vec<Shell> = ALL_SHELLS
        .iter()
        .copied()
        .filter(|sh| sh.rc_path(home).exists())
        .collect();

    // The login shell counts even before its rc file exists.
    if let Some(cur) = current {
        if !found.contains(&cur) {
            found.push(cur);
        }
    }

    found
}

/// Result of configuring one rc file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcOutcome {
    /// A managed block was added for the first time.
    Added,
    /// An existing managed block was replaced with current content.
    Updated,
    /// The existing block already matched; nothing written.
    Unchanged,
}

/// Replace (or insert) the managed block in `content`, returning the new text.
///
/// Idempotent for every marker arrangement, not just well-formed ones: a user
/// who hand-edits the rc can leave an orphaned or reversed marker behind, and if
/// those fell through to the append path the file would grow on every run.
///
/// Only lines Synapse wrote are ever removed. Each well-formed `START…END` pair
/// is dropped along with its body, stray unpaired markers are dropped on their
/// own, and *everything else is preserved verbatim* — including lines sitting
/// between two managed blocks, which a dotfile merged from two machines will
/// have. The single replacement block is spliced in where the first marker stood.
///
/// Exposed separately from the file I/O so the rewriting logic is directly
/// testable.
pub fn splice_block(content: &str, block_body: &str) -> String {
    let managed = format!(
        "{MARKER_START}\n{}{MARKER_END}\n",
        ensure_trailing_nl(block_body)
    );

    let (kept, had_marker) = remove_managed_lines(content);

    if !had_marker {
        // No marker anywhere: append a fresh block.
        let mut out = String::with_capacity(content.len() + managed.len() + 2);
        out.push_str(content);
        if !content.is_empty() {
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&managed);
        return out;
    }

    let mut out = String::with_capacity(content.len() + managed.len());
    for item in &kept {
        match item {
            Kept::User(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Kept::BlockSlot => out.push_str(&managed),
        }
    }
    out
}

/// A retained line, or the position where the managed block belongs.
enum Kept<'a> {
    User(&'a str),
    BlockSlot,
}

/// Strip every line Synapse wrote, keeping all other lines in order.
///
/// Returns the retained lines — with a single [`Kept::BlockSlot`] marking where
/// the first managed region started — and whether any marker was found.
///
/// A `START` only opens a block when a matching `END` appears later; otherwise
/// it is a stray marker and just its own line is dropped. Without that check a
/// trailing orphaned `START` would swallow every line after it.
fn remove_managed_lines(content: &str) -> (Vec<Kept<'_>>, bool) {
    let lines: Vec<&str> = content.lines().collect();

    // Body ranges belonging to well-formed START…END pairs, matched greedily so
    // a duplicated START inside a pair is treated as part of that pair's body.
    let mut in_pair = vec![false; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim() == MARKER_START {
            if let Some(end) = (i + 1..lines.len()).find(|&j| lines[j].trim() == MARKER_END) {
                for slot in in_pair.iter_mut().take(end + 1).skip(i) {
                    *slot = true;
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }

    let mut kept: Vec<Kept<'_>> = Vec::with_capacity(lines.len());
    let mut had_marker = false;
    let mut slot_placed = false;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_marker = trimmed == MARKER_START || trimmed == MARKER_END;

        if is_marker {
            had_marker = true;
        }

        // Managed content: a paired block's lines, or a stray marker line.
        if in_pair[idx] || is_marker {
            if !slot_placed {
                kept.push(Kept::BlockSlot);
                slot_placed = true;
            }
            continue;
        }

        kept.push(Kept::User(line));
    }

    (tidy(kept), had_marker)
}

/// Collapse consecutive and trailing blank lines left behind by a removed
/// block, so repeated install/uninstall cycles do not accumulate whitespace.
fn tidy(kept: Vec<Kept<'_>>) -> Vec<Kept<'_>> {
    let mut out: Vec<Kept<'_>> = Vec::with_capacity(kept.len());
    for item in kept {
        let is_blank = matches!(&item, Kept::User(t) if t.trim().is_empty());
        let prev_blank = matches!(out.last(), Some(Kept::User(t)) if t.trim().is_empty());
        if is_blank && (prev_blank || out.is_empty()) {
            continue;
        }
        out.push(item);
    }
    while matches!(out.last(), Some(Kept::User(t)) if t.trim().is_empty()) {
        out.pop();
    }
    out
}

fn ensure_trailing_nl(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

/// Remove the managed block from `content`, leaving user content untouched.
///
/// Same line-level discipline as [`splice_block`]: only lines Synapse wrote are
/// removed, so content between two managed blocks survives.
pub fn strip_block(content: &str) -> String {
    let (kept, had_marker) = remove_managed_lines(content);
    if !had_marker {
        return content.to_string();
    }

    // `tidy` keeps a blank line that sat before the block, since the slot itself
    // is not blank. Dropping it here keeps install → uninstall → install a
    // round trip instead of accreting a blank line per cycle.
    let mut user: Vec<&str> = kept
        .iter()
        .filter_map(|item| match item {
            Kept::User(text) => Some(*text),
            Kept::BlockSlot => None,
        })
        .collect();
    while user.last().is_some_and(|l| l.trim().is_empty()) {
        user.pop();
    }

    if user.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(content.len());
    for line in user {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The full managed body for `shell`: PATH setup plus any completion loader.
///
/// Shared by [`configure_rc`] and [`plan_rc`] so a dry run cannot predict a
/// different result than the real run produces.
pub fn managed_body(shell: Shell, completion_line: Option<&str>) -> String {
    let mut body = shell.path_snippet();
    if let Some(line) = completion_line {
        body.push_str(line);
        if !line.ends_with('\n') {
            body.push('\n');
        }
    }
    body
}

/// Read `shell`'s rc and report what [`configure_rc`] *would* do, writing
/// nothing.
///
/// A missing rc file counts as empty, matching `configure_rc`.
pub fn plan_rc(shell: Shell, home: &Path, completion_line: Option<&str>) -> io::Result<RcOutcome> {
    let existing = match fs::read_to_string(shell.rc_path(home)) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    let body = managed_body(shell, completion_line);
    let updated = splice_block(&existing, &body);

    Ok(if updated == existing {
        RcOutcome::Unchanged
    } else if existing.contains(MARKER_START) || existing.contains(MARKER_END) {
        RcOutcome::Updated
    } else {
        RcOutcome::Added
    })
}

/// Write the managed block into `shell`'s rc file under `home`.
///
/// Creates parent directories and the rc file if needed (fish keeps its config
/// in a nested directory that may not exist yet).
pub fn configure_rc(
    shell: Shell,
    home: &Path,
    completion_line: Option<&str>,
) -> io::Result<RcOutcome> {
    let rc = shell.rc_path(home);
    if let Some(parent) = rc.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = match fs::read_to_string(&rc) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    let body = managed_body(shell, completion_line);

    // Any marker means a managed region already exists — including a malformed
    // one left by hand-editing, which splice_block repairs rather than appends
    // past. Must match plan_rc's classification exactly.
    let had_block = existing.contains(MARKER_START) || existing.contains(MARKER_END);
    let updated = splice_block(&existing, &body);

    if updated == existing {
        return Ok(RcOutcome::Unchanged);
    }

    // Atomic replace so a crash cannot truncate the user's rc file.
    let tmp = rc.with_extension("synapse-tmp");
    fs::write(&tmp, updated.as_bytes())?;
    fs::rename(&tmp, &rc)?;

    Ok(if had_block {
        RcOutcome::Updated
    } else {
        RcOutcome::Added
    })
}

/// Directory where completion scripts are written for `shell`.
pub fn completion_dir(shell: Shell, home: &Path) -> PathBuf {
    match shell {
        // Sourced explicitly from the managed rc block.
        Shell::Bash => home.join(".local/share/bash-completion/completions"),
        Shell::Zsh => home.join(".local/share/zsh/site-functions"),
        // fish autoloads from this path; no rc line needed.
        Shell::Fish => home.join(".config/fish/completions"),
    }
}

/// Filename clap_complete produces for each shell's script.
///
/// These are clap_complete's own names, not ours — `configure_rc` sources them
/// by path, so they must match exactly. `write_completions` asserts this.
pub fn completion_file(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => "synapse.bash",
        Shell::Zsh => "_synapse",
        Shell::Fish => "synapse.fish",
    }
}

/// The rc line needed to load completions, if the shell needs one.
///
/// fish autoloads `~/.config/fish/completions`, so it returns `None`.
pub fn completion_rc_line(shell: Shell) -> Option<String> {
    match shell {
        Shell::Bash => Some(
            "# Synapse completions\n\
             if [ -r \"$HOME/.local/share/bash-completion/completions/synapse.bash\" ]; then\n\
             \x20 . \"$HOME/.local/share/bash-completion/completions/synapse.bash\"\n\
             fi\n"
                .to_string(),
        ),
        Shell::Zsh => Some(
            "# Synapse completions\n\
             fpath=(\"$HOME/.local/share/zsh/site-functions\" $fpath)\n\
             autoload -Uz compinit && compinit -u\n"
                .to_string(),
        ),
        Shell::Fish => None,
    }
}

/// Verify an installed binary is executable by running `<bin> --version`.
///
/// Returns the trimmed first line of output on success. Some tools print their
/// banner to stderr, so that is used as a fallback before declaring failure.
pub fn verify_binary(bin: &str) -> Result<String, String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("{bin}: {e}"))?;

    if !out.status.success() {
        return Err(format!("{bin}: exited {}", out.status.code().unwrap_or(-1)));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    if let Some(line) = stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        return Ok(line.to_string());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    match stderr
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        Some(line) => Ok(line.to_string()),
        None => Err(format!("{bin}: no version output")),
    }
}

/// Build the post-install next-steps message.
///
/// `installed` is `(package, version)`; `rc_files` are the rc paths touched.
pub fn next_steps(
    installed: &[(String, String)],
    rc_files: &[PathBuf],
    shell: Option<Shell>,
) -> String {
    let mut out = String::new();

    if installed.is_empty() {
        out.push_str("Nothing was installed.\n");
        return out;
    }

    out.push_str("Installed:\n");
    for (name, version) in installed {
        out.push_str(&format!("  {name} {version}\n"));
    }

    if !rc_files.is_empty() {
        out.push_str("\nUpdated shell config:\n");
        for rc in rc_files {
            out.push_str(&format!("  {}\n", rc.display()));
        }

        let reload = match shell {
            Some(Shell::Fish) => "exec fish".to_string(),
            Some(sh) => format!("source ~/{}", sh.rc_relative()),
            None => "restart your shell".to_string(),
        };
        out.push_str(&format!("\nStart a new shell, or run:\n  {reload}\n"));
    }

    out.push_str("\nThen try:\n");
    for (name, _) in installed {
        out.push_str(&format!("  {name} --version\n"));
    }
    out.push_str("  synapse doctor        # verify the install\n");
    out.push_str("  synapse update --all  # update everything later\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synapse-shell-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rc_paths_match_shell_conventions() {
        let home = Path::new("/home/u");
        assert_eq!(Shell::Bash.rc_path(home), home.join(".bashrc"));
        assert_eq!(Shell::Zsh.rc_path(home), home.join(".zshrc"));
        assert_eq!(
            Shell::Fish.rc_path(home),
            home.join(".config/fish/config.fish")
        );
    }

    #[test]
    fn splice_appends_when_absent() {
        let out = splice_block("export FOO=1\n", "PATH=x\n");
        assert!(
            out.starts_with("export FOO=1\n"),
            "existing content preserved"
        );
        assert!(out.contains(MARKER_START));
        assert!(out.contains("PATH=x"));
        assert!(out.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn splice_is_idempotent() {
        // The core acceptance criterion: running twice must not duplicate.
        let first = splice_block("export FOO=1\n", "PATH=x\n");
        let second = splice_block(&first, "PATH=x\n");
        assert_eq!(first, second, "second splice changed the file");
        assert_eq!(
            second.matches(MARKER_START).count(),
            1,
            "duplicate managed block"
        );
    }

    #[test]
    fn splice_replaces_stale_block_in_place() {
        let old = splice_block("keep-before\n", "PATH=old\n");
        let new = splice_block(&old, "PATH=new\n");
        assert!(new.contains("PATH=new"));
        assert!(!new.contains("PATH=old"), "stale content left behind");
        assert_eq!(new.matches(MARKER_START).count(), 1);
        assert!(new.starts_with("keep-before\n"), "user content clobbered");
    }

    #[test]
    fn splice_preserves_content_after_block() {
        let content = format!("before\n\n{MARKER_START}\nPATH=old\n{MARKER_END}\nafter\n");
        let out = splice_block(&content, "PATH=new\n");
        assert!(out.contains("before"));
        assert!(out.contains("after"), "trailing user content dropped");
        assert!(out.contains("PATH=new"));
        assert!(!out.contains("PATH=old"));
    }

    #[test]
    fn splice_handles_missing_trailing_newline() {
        let out = splice_block("no-newline-at-eof", "PATH=x\n");
        assert!(out.contains("no-newline-at-eof\n"), "newline not inserted");
        assert!(out.contains(MARKER_START));
    }

    #[test]
    fn splice_into_empty_file() {
        let out = splice_block("", "PATH=x\n");
        assert!(out.starts_with(MARKER_START), "no leading blank padding");
        assert_eq!(out.matches(MARKER_START).count(), 1);
    }

    /// Every non-managed line in the input must appear in the output, counted
    /// per line across the whole file.
    ///
    /// The earlier "user content preserved" check only looked outside the
    /// outermost markers, so it passed while lines *between* two managed blocks
    /// were being deleted. Counting every KEEP line is what catches that.
    fn assert_all_user_lines_survive(label: &str, before: &str, after: &str) {
        for line in before.lines() {
            let t = line.trim();
            if t.is_empty() || t == MARKER_START || t == MARKER_END {
                continue;
            }
            // Managed body lines are ours to remove; KEEP-tagged lines are not.
            if !t.starts_with("KEEP") {
                continue;
            }
            let expected = before.lines().filter(|l| l.trim() == t).count();
            let actual = after.lines().filter(|l| l.trim() == t).count();
            assert_eq!(
                actual, expected,
                "{label}: user line {t:?} count {expected} → {actual}\n\
                 --- before ---\n{before}\n--- after ---\n{after}"
            );
        }
    }

    /// Content between two managed blocks must survive. This is the shape a
    /// dotfile merged from two machines has, and it was silently destroyed by
    /// spanning one region from the first marker to the last.
    #[test]
    fn splice_preserves_content_between_two_blocks() {
        let before = format!(
            "KEEP-header\n\
             {MARKER_START}\nold-a\n{MARKER_END}\n\
             KEEP-alias-deploy\n\
             KEEP-secret-token-path\n\
             {MARKER_START}\nold-b\n{MARKER_END}\n\
             KEEP-alias-gs\n"
        );

        let after = splice_block(&before, "PATH=new\n");
        assert_all_user_lines_survive("between two blocks", &before, &after);

        // And it still converges to a single managed block.
        assert_eq!(after.matches(MARKER_START).count(), 1);
        assert_eq!(after.matches(MARKER_END).count(), 1);
        assert!(!after.contains("old-a") && !after.contains("old-b"));
        assert!(after.contains("PATH=new"));

        // Idempotent on the repaired file.
        let again = splice_block(&after, "PATH=new\n");
        assert_eq!(after, again, "not idempotent after repair");
    }

    /// Same guarantee across every malformed arrangement, with interleaved user
    /// lines in each gap where content could be swallowed.
    #[test]
    fn splice_never_deletes_user_lines_in_any_shape() {
        let cases: [(&str, String); 7] = [
            (
                "two blocks with content between",
                format!(
                    "KEEP-1\n{MARKER_START}\na\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nb\n{MARKER_END}\nKEEP-3\n"
                ),
            ),
            (
                "three blocks",
                format!(
                    "KEEP-1\n{MARKER_START}\na\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nb\n{MARKER_END}\nKEEP-3\n{MARKER_START}\nc\n{MARKER_END}\nKEEP-4\n"
                ),
            ),
            (
                "orphaned end then user lines then block",
                format!(
                    "KEEP-1\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nb\n{MARKER_END}\nKEEP-3\n"
                ),
            ),
            (
                "orphaned start after a block",
                format!(
                    "KEEP-1\n{MARKER_START}\na\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nKEEP-3\n"
                ),
            ),
            (
                "reversed markers around user content",
                format!("KEEP-1\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nKEEP-3\n"),
            ),
            (
                "well-formed single block",
                format!("KEEP-1\n{MARKER_START}\na\n{MARKER_END}\nKEEP-2\n"),
            ),
            ("no markers at all", "KEEP-1\nKEEP-2\n".to_string()),
        ];

        for (label, before) in cases {
            let after = splice_block(&before, "PATH=x\n");
            assert_all_user_lines_survive(label, &before, &after);

            assert_eq!(
                after.matches(MARKER_START).count(),
                1,
                "{label}: expected exactly one start marker"
            );
            assert_eq!(
                after.matches(MARKER_END).count(),
                1,
                "{label}: expected exactly one end marker"
            );

            let twice = splice_block(&after, "PATH=x\n");
            assert_eq!(after, twice, "{label}: not idempotent");
        }
    }

    /// `strip_block` must also only remove lines Synapse wrote.
    #[test]
    fn strip_block_preserves_content_between_blocks() {
        let before = format!(
            "KEEP-1\n{MARKER_START}\na\n{MARKER_END}\nKEEP-2\n{MARKER_START}\nb\n{MARKER_END}\nKEEP-3\n"
        );
        let after = strip_block(&before);

        assert_all_user_lines_survive("strip between blocks", &before, &after);
        assert!(!after.contains(MARKER_START), "markers left behind");
        assert!(!after.contains(MARKER_END), "markers left behind");
        assert!(
            !after.contains('a') || !after.contains("\na\n"),
            "body left behind"
        );
    }

    /// install → uninstall → install must not lose user content or drift.
    #[test]
    fn install_uninstall_cycle_is_lossless() {
        let original = "KEEP-1\nKEEP-2\n".to_string();

        let installed = splice_block(&original, "PATH=x\n");
        let uninstalled = strip_block(&installed);
        assert_all_user_lines_survive("cycle", &original, &uninstalled);

        let reinstalled = splice_block(&uninstalled, "PATH=x\n");
        assert_eq!(
            installed, reinstalled,
            "install/uninstall/install did not return to the same content"
        );
    }

    /// Every malformed marker arrangement must converge after one splice,
    /// otherwise repeated `setup-shell` runs grow the rc file without bound.
    #[test]
    fn splice_is_idempotent_for_malformed_markers() {
        let cases: [(&str, String); 5] = [
            // The shape QA reproduced: user deleted the block but left the end
            // marker. Previously appended a new block on every single run.
            ("orphaned end marker", format!("# USER\n{MARKER_END}\n")),
            ("orphaned start marker", format!("# USER\n{MARKER_START}\n")),
            (
                "reversed order",
                format!("# USER\n{MARKER_END}\nstale\n{MARKER_START}\n"),
            ),
            (
                "duplicated blocks",
                format!(
                    "# USER\n{MARKER_START}\nold-a\n{MARKER_END}\n{MARKER_START}\nold-b\n{MARKER_END}\n"
                ),
            ),
            (
                "duplicated start markers",
                format!("# USER\n{MARKER_START}\n{MARKER_START}\nold\n{MARKER_END}\n"),
            ),
        ];

        for (name, original) in cases {
            let first = splice_block(&original, "PATH=x\n");
            let second = splice_block(&first, "PATH=x\n");

            assert_eq!(
                first, second,
                "{name}: not idempotent — rc would grow on every run"
            );
            assert_eq!(
                first.matches(MARKER_START).count(),
                1,
                "{name}: expected exactly one start marker, got {first:?}"
            );
            assert_eq!(
                first.matches(MARKER_END).count(),
                1,
                "{name}: expected exactly one end marker, got {first:?}"
            );
            assert!(first.contains("# USER"), "{name}: user content was lost");
            assert!(first.contains("PATH=x"), "{name}: managed body missing");
            // Content from well-formed block bodies must be removed. Content
            // between two *unpaired* markers is user data and must survive.
            assert!(
                !first.contains("old-a") && !first.contains("old-b"),
                "{name}: block body was kept: {first:?}"
            );
        }
    }

    /// Repeated splices must not grow the file, for any marker shape. This is
    /// the property the unbounded-growth bug violated.
    #[test]
    fn repeated_splices_do_not_grow_the_file() {
        let shapes = [
            format!("# USER\n{MARKER_END}\n"),
            format!("# USER\n{MARKER_START}\n"),
            format!("# USER\n{MARKER_START}\nbody\n{MARKER_END}\n"),
            "# USER\n".to_string(),
        ];

        for shape in shapes {
            let once = splice_block(&shape, "PATH=x\n");
            let mut current = once.clone();
            for run in 2..=5 {
                current = splice_block(&current, "PATH=x\n");
                assert_eq!(
                    current.len(),
                    once.len(),
                    "run {run} changed file size for {shape:?} \
                     ({} → {} bytes)",
                    once.len(),
                    current.len()
                );
            }
        }
    }

    #[test]
    fn configure_rc_adds_then_reports_unchanged() {
        let home = tmpdir("cfg");
        fs::write(home.join(".bashrc"), "export EXISTING=1\n").unwrap();

        let first = configure_rc(Shell::Bash, &home, None).unwrap();
        assert_eq!(first, RcOutcome::Added);

        let second = configure_rc(Shell::Bash, &home, None).unwrap();
        assert_eq!(second, RcOutcome::Unchanged, "re-run rewrote the file");

        let content = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert_eq!(content.matches(MARKER_START).count(), 1);
        assert!(content.contains("export EXISTING=1"), "user content lost");

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn configure_rc_updates_when_body_changes() {
        let home = tmpdir("upd");
        configure_rc(Shell::Bash, &home, None).unwrap();
        let outcome = configure_rc(Shell::Bash, &home, Some("# completions\n")).unwrap();
        assert_eq!(outcome, RcOutcome::Updated);

        let content = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert_eq!(content.matches(MARKER_START).count(), 1);
        assert!(content.contains("# completions"));

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn configure_rc_creates_nested_fish_config() {
        let home = tmpdir("fish");
        let outcome = configure_rc(Shell::Fish, &home, None).unwrap();
        assert_eq!(outcome, RcOutcome::Added);

        let rc = home.join(".config/fish/config.fish");
        assert!(rc.exists(), "fish config not created");
        let content = fs::read_to_string(&rc).unwrap();
        assert!(content.contains("fish_add_path"), "wrong syntax for fish");
        assert!(
            !content.contains("export PATH"),
            "POSIX syntax leaked into fish"
        );

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn configure_rc_leaves_no_temp_file() {
        let home = tmpdir("tmp");
        configure_rc(Shell::Zsh, &home, None).unwrap();
        assert!(
            !home.join(".zshrc.synapse-tmp").exists(),
            "temp file leaked"
        );
        fs::remove_dir_all(&home).ok();
    }

    /// `plan_rc` exists to answer "would this change anything?". If it ever
    /// disagrees with `configure_rc`, `--dry-run` is lying to the user.
    #[test]
    fn plan_rc_agrees_with_configure_rc() {
        let cases: [(&str, String); 5] = [
            ("unconfigured", "# USER\n".to_string()),
            ("empty file", String::new()),
            (
                "well-formed block",
                format!("# USER\n{MARKER_START}\nstale\n{MARKER_END}\n"),
            ),
            ("orphaned end marker", format!("# USER\n{MARKER_END}\n")),
            ("orphaned start marker", format!("# USER\n{MARKER_START}\n")),
        ];

        for (name, initial) in cases {
            let home = tmpdir("plan");
            fs::write(home.join(".bashrc"), &initial).unwrap();

            let predicted = plan_rc(Shell::Bash, &home, None).unwrap();
            let actual = configure_rc(Shell::Bash, &home, None).unwrap();
            assert_eq!(
                predicted, actual,
                "{name}: dry-run predicted the wrong outcome"
            );

            // And once configured, both must agree that nothing is left to do.
            let predicted_again = plan_rc(Shell::Bash, &home, None).unwrap();
            assert_eq!(
                predicted_again,
                RcOutcome::Unchanged,
                "{name}: dry-run should report no change on a configured rc"
            );

            fs::remove_dir_all(&home).ok();
        }
    }

    /// A dry run must not create, modify, or delete anything.
    #[test]
    fn plan_rc_writes_nothing() {
        let home = tmpdir("planpure");
        let rc = home.join(".bashrc");
        fs::write(&rc, "# USER\n").unwrap();
        let before = fs::read_to_string(&rc).unwrap();

        let outcome = plan_rc(Shell::Bash, &home, None).unwrap();
        assert_eq!(outcome, RcOutcome::Added);

        assert_eq!(fs::read_to_string(&rc).unwrap(), before, "rc was modified");
        assert!(
            !home.join(".bashrc.synapse-tmp").exists(),
            "temp file created"
        );

        fs::remove_dir_all(&home).ok();
    }

    /// A missing rc must be planned as an addition, not an error.
    #[test]
    fn plan_rc_handles_missing_rc_file() {
        let home = tmpdir("planmissing");
        assert_eq!(plan_rc(Shell::Fish, &home, None).unwrap(), RcOutcome::Added);
        assert!(
            !home.join(".config/fish/config.fish").exists(),
            "planning created the rc file"
        );
        fs::remove_dir_all(&home).ok();
    }

    /// Both paths must build the same managed body, including the completion
    /// loader — otherwise `Unchanged` comparisons drift apart.
    #[test]
    fn managed_body_includes_completion_loader() {
        let with = managed_body(Shell::Bash, completion_rc_line(Shell::Bash).as_deref());
        let without = managed_body(Shell::Bash, None);
        assert!(with.contains("synapse.bash"), "completion loader missing");
        assert!(with.starts_with(&without), "path snippet must come first");
    }

    #[test]
    fn detect_finds_shell_with_existing_rc() {
        let home = tmpdir("det");
        fs::write(home.join(".zshrc"), "").unwrap();
        let found = detect_shells(&home, None);
        assert_eq!(found, vec![Shell::Zsh]);
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn detect_includes_login_shell_without_rc() {
        let home = tmpdir("login");
        // No rc files exist at all.
        let found = detect_shells(&home, Some("/bin/bash"));
        assert_eq!(found, vec![Shell::Bash]);
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn detect_does_not_duplicate_login_shell() {
        let home = tmpdir("dup");
        fs::write(home.join(".bashrc"), "").unwrap();
        let found = detect_shells(&home, Some("/bin/bash"));
        assert_eq!(found, vec![Shell::Bash], "login shell counted twice");
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn detect_ignores_unknown_shell() {
        let home = tmpdir("unknown");
        let found = detect_shells(&home, Some("/usr/bin/nu"));
        assert!(
            found.is_empty(),
            "unsupported shell should not be configured"
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn fish_needs_no_completion_rc_line() {
        // fish autoloads its completions directory.
        assert!(completion_rc_line(Shell::Fish).is_none());
        assert!(completion_rc_line(Shell::Bash).is_some());
        assert!(completion_rc_line(Shell::Zsh).is_some());
    }

    #[test]
    fn completion_filenames_match_clap_complete_output() {
        // These are the names clap_complete actually writes; the rc lines in
        // completion_rc_line source them by exact path. bash gets a .bash
        // suffix — assuming a bare "synapse" made the rc source a missing file.
        assert_eq!(completion_file(Shell::Zsh), "_synapse");
        assert_eq!(completion_file(Shell::Bash), "synapse.bash");
        assert_eq!(completion_file(Shell::Fish), "synapse.fish");
    }

    #[test]
    fn bash_rc_line_references_the_generated_filename() {
        // Regression guard for the mismatch above: the sourced path must be the
        // file we actually generate, or completions silently never load.
        let line = completion_rc_line(Shell::Bash).unwrap();
        assert!(
            line.contains(completion_file(Shell::Bash)),
            "bash rc line does not source the generated script: {line}"
        );
    }

    #[test]
    fn zsh_rc_line_uses_the_completion_dir() {
        let home = Path::new("/home/u");
        let dir = completion_dir(Shell::Zsh, home);
        let tail = dir.strip_prefix(home).unwrap().to_str().unwrap();
        let line = completion_rc_line(Shell::Zsh).unwrap();
        assert!(
            line.contains(tail),
            "zsh fpath does not point at completion_dir ({tail}): {line}"
        );
    }

    #[test]
    fn verify_binary_reports_missing() {
        let err = verify_binary("synapse-definitely-not-a-real-binary").unwrap_err();
        assert!(err.contains("synapse-definitely-not-a-real-binary"));
    }

    #[test]
    fn verify_binary_never_returns_empty_success() {
        // `env` exists on every supported platform and supports --version.
        if let Ok(line) = verify_binary("env") {
            assert!(!line.is_empty(), "empty version string returned as success");
        }
    }

    #[test]
    fn next_steps_lists_packages_and_reload() {
        let installed = vec![
            ("herdr".to_string(), "0.7.5".to_string()),
            ("omp".to_string(), "17.2.2".to_string()),
        ];
        let rcs = vec![PathBuf::from("/home/u/.zshrc")];
        let msg = next_steps(&installed, &rcs, Some(Shell::Zsh));

        assert!(msg.contains("herdr 0.7.5"));
        assert!(msg.contains("omp 17.2.2"));
        assert!(msg.contains(".zshrc"), "rc file not mentioned");
        assert!(msg.contains("source ~/.zshrc"), "no reload instruction");
        assert!(msg.contains("synapse doctor"), "no verification hint");
    }

    #[test]
    fn next_steps_uses_fish_reload_syntax() {
        let installed = vec![("herdr".to_string(), "0.7.5".to_string())];
        let rcs = vec![PathBuf::from("/home/u/.config/fish/config.fish")];
        let msg = next_steps(&installed, &rcs, Some(Shell::Fish));
        assert!(msg.contains("exec fish"));
        assert!(
            !msg.contains("source ~/"),
            "POSIX reload leaked into fish message"
        );
    }

    #[test]
    fn next_steps_handles_nothing_installed() {
        let msg = next_steps(&[], &[], None);
        assert!(msg.contains("Nothing was installed"));
    }
}

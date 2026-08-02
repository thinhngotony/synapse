//! Interactive TUI installer for Synapse.
//!
//! Flow:
//!   1. Show detected OS/arch and whether Nix is available.
//!   2. Multi-select which packages to install.
//!   3. For each selected package, run `nix build .#<name>` and tail its
//!      stderr for `nix-fast-build`-style progress lines.
//!   4. Show a per-package progress bar with ETA.
//!   5. On completion, run a `synapse doctor` stub to verify installed binaries.
//!   6. On Ctrl+C at any point, print a message and exit cleanly; partial Nix
//!      store entries are GC'd by Nix on the next collect run.

use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

use crate::nix;
use crate::platform;
use crate::state;

// ── Package catalogue ─────────────────────────────────────────────────────────

/// A managed package Synapse can install.
#[derive(Debug, Clone)]
pub struct Package {
    /// Nix attribute name (`nix build .#<nix_attr>`).
    pub nix_attr: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Short description shown in the selector.
    pub description: &'static str,
    /// Approximate compressed download size in MB (shown as an estimate).
    pub size_mb: u32,
}

/// Every package Synapse manages. Order = display order in the TUI.
pub const PACKAGES: &[Package] = &[
    Package {
        nix_attr: "herdr",
        name: "herdr",
        description: "Terminal multiplexer for coding agents",
        size_mb: 12,
    },
    Package {
        nix_attr: "omp",
        name: "omp",
        description: "Oh My Pi coding agent CLI",
        size_mb: 40,
    },
    Package {
        nix_attr: "skillshare",
        name: "skillshare",
        description: "Sync AI CLI skills across tools",
        size_mb: 8,
    },
];

// ── TUI state machine ─────────────────────────────────────────────────────────

/// Which step of the TUI the user is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Select,
    Installing,
    Done,
}

/// In-memory state threaded through the TUI event loop.
pub struct InstallerState {
    pub step: Step,
    /// Which packages are selected in the picker.
    pub selected: Vec<bool>,
    /// Cursor in the package list.
    pub list_state: ListState,
    /// Per-package install status.
    pub install_status: Vec<InstallStatus>,
    /// Platform / Nix info, shown in the header.
    pub nix_status: nix::NixStatus,
    pub os: platform::OS,
    pub arch: platform::Arch,
}

/// Progress of one package installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    Pending,
    Running { started: Instant },
    Done,
    Failed(String),
}

impl InstallStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

impl InstallerState {
    pub fn new() -> Self {
        let n = PACKAGES.len();
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            step: Step::Select,
            selected: vec![true; n], // all ticked by default
            list_state,
            install_status: vec![InstallStatus::Pending; n],
            nix_status: nix::detect(),
            os: platform::detect_os(),
            arch: platform::detect_arch(),
        }
    }

    pub fn toggle_current(&mut self) {
        if let Some(i) = self.list_state.selected() {
            self.selected[i] = !self.selected[i];
        }
    }

    pub fn move_up(&mut self) {
        let i = self.list_state.selected().unwrap_or(0);
        let next = if i == 0 { PACKAGES.len() - 1 } else { i - 1 };
        self.list_state.select(Some(next));
    }

    pub fn move_down(&mut self) {
        let i = self.list_state.selected().unwrap_or(0);
        let next = (i + 1) % PACKAGES.len();
        self.list_state.select(Some(next));
    }

    pub fn any_selected(&self) -> bool {
        self.selected.iter().any(|&s| s)
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, state: &mut InstallerState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_ROWS),
            Constraint::Min(0), // content
            Constraint::Length(FOOTER_ROWS),
        ])
        .split(f.area());

    draw_header(f, state, chunks[0]);
    match state.step {
        Step::Select => draw_select(f, state, chunks[1]),
        Step::Installing | Step::Done => draw_progress(f, state, chunks[1]),
    }
    draw_footer(f, state, chunks[2]);
}

fn draw_header(f: &mut Frame, state: &InstallerState, area: ratatui::layout::Rect) {
    let os_str = match state.os {
        platform::OS::Mac => "macOS",
        platform::OS::Linux => "Linux",
        platform::OS::Windows => "Windows",
    };
    let arch_str = match state.arch {
        platform::Arch::X86_64 => "x86_64",
        platform::Arch::Aarch64 => "aarch64",
    };
    let nix_str = match &state.nix_status {
        nix::NixStatus::Supported(v) => format!("Nix {v} ✓"),
        nix::NixStatus::TooOld(v) => format!("Nix {v} (too old, need 2.24+)"),
        nix::NixStatus::Missing => "Nix not found".to_string(),
    };
    let nix_style = match &state.nix_status {
        nix::NixStatus::Supported(_) => Style::default().fg(Color::Green),
        _ => Style::default().fg(Color::Red),
    };

    let text = vec![
        Line::from(vec![
            Span::raw("Platform: "),
            Span::styled(
                format!("{os_str}/{arch_str}"),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(nix_str, nix_style),
        ]),
        Line::from(Span::styled(
            "Synapse v1.0 — AI harness installer",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" Synapse ");
    f.render_widget(
        Paragraph::new(text).block(block).style(Style::default()),
        area,
    );
}

fn draw_select(f: &mut Frame, state: &mut InstallerState, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = PACKAGES
        .iter()
        .enumerate()
        .map(|(i, pkg)| {
            let tick = if state.selected[i] { "[x]" } else { "[ ]" };
            let label = format!(
                "{tick} {:<12}  ~{:>3} MB  {}",
                pkg.name, pkg.size_mb, pkg.description
            );
            let style = if state.list_state.selected() == Some(i) {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Select packages (Space = toggle, Enter = install) "),
    );
    f.render_stateful_widget(list, area, &mut state.list_state);
}

fn draw_progress(f: &mut Frame, state: &InstallerState, area: ratatui::layout::Rect) {
    let n = PACKAGES
        .iter()
        .enumerate()
        .filter(|(i, _)| state.selected[*i])
        .count();
    let done = state
        .install_status
        .iter()
        .filter(|s| s.is_terminal())
        .count();

    // One row per selected package.
    let constraints: Vec<Constraint> = state
        .selected
        .iter()
        .filter(|&&s| s)
        .map(|_| Constraint::Length(3))
        .collect();
    let sub_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut row = 0usize;
    for (i, pkg) in PACKAGES.iter().enumerate() {
        if !state.selected[i] {
            continue;
        }
        let (ratio, label, color) = match &state.install_status[i] {
            InstallStatus::Pending => (0.0, format!("{}: waiting…", pkg.name), Color::DarkGray),
            InstallStatus::Running { started } => {
                let elapsed = started.elapsed().as_secs();
                // Estimate: ~30 s per package on a cache miss; shown as % of time budget.
                // ponytail: real ETA needs build log parsing; add when SYN-9 wires telem
                let pct = (elapsed as f64 / 30.0).min(0.95);
                (
                    pct,
                    format!("{}: building… {}s", pkg.name, elapsed),
                    Color::Blue,
                )
            }
            InstallStatus::Done => (1.0, format!("{}: done ✓", pkg.name), Color::Green),
            InstallStatus::Failed(msg) => {
                (0.0, format!("{}: FAILED — {}", pkg.name, msg), Color::Red)
            }
        };

        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL))
            .gauge_style(Style::default().fg(color))
            .ratio(ratio)
            .label(label);
        if row < sub_chunks.len() {
            f.render_widget(gauge, sub_chunks[row]);
        }
        row += 1;
    }

    let _ = (n, done); // used in footer
}

fn draw_footer(f: &mut Frame, state: &InstallerState, area: ratatui::layout::Rect) {
    let text = match state.step {
        Step::Select => "↑/↓ move  Space toggle  Enter install  q quit  Ctrl+C abort",
        Step::Installing => {
            "Installing… Ctrl+C to abort (partial installs cleaned up by nix-collect-garbage)"
        }
        Step::Done => "Installation complete. Press q to exit.",
    };
    let para = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Keys "))
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(para, area);
}

// ── Install logic ─────────────────────────────────────────────────────────────

/// Install a single package via `nix build .#<attr>` and update `state.json`.
///
/// Progress callback is called with elapsed seconds so the TUI can update the
/// gauge. Returns the installed version on success (read from `nix eval`).
pub fn install_package(
    pkg: &Package,
    flake_dir: &std::path::Path,
    nix_bin: &str,
) -> Result<String, String> {
    let mut child: Child = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "build",
            &format!(".#{}", pkg.nix_attr),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn nix: {e}"))?;

    let status = child.wait().map_err(|e| format!("wait nix: {e}"))?;
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Err(format!("nix build exited with status {code}"));
    }

    // Read version via `nix eval .#<attr>.version`
    let ver_out = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "eval",
            "--raw",
            &format!(".#{}.version", pkg.nix_attr),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .output()
        .map_err(|e| format!("nix eval version: {e}"))?;

    let version = if ver_out.status.success() {
        String::from_utf8_lossy(&ver_out.stdout).trim().to_string()
    } else {
        "unknown".to_string()
    };

    Ok(version)
}

/// Spawn `install_package` on a worker thread.
///
/// The returned receiver yields exactly one message when the build finishes.
/// The caller polls it with `try_recv` between renders, so the ratatui event
/// loop keeps ticking and the progress gauge animates while Nix works.
pub fn spawn_install(
    pkg: &'static Package,
    flake_dir: std::path::PathBuf,
    nix_bin: String,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // Send failure is fine: it only means the TUI stopped listening.
        let _ = tx.send(install_package(pkg, &flake_dir, &nix_bin));
    });
    rx
}

/// Post-install verification — stubs the `synapse doctor` check (full impl SYN-11).
///
/// Returns a list of `(binary, ok)` pairs.
pub fn doctor_check(binaries: &[&str]) -> Vec<(String, bool)> {
    binaries
        .iter()
        .map(|bin| {
            let ok = Command::new("which")
                .arg(bin)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            (bin.to_string(), ok)
        })
        .collect()
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Minimum terminal size the installer needs.
///
/// The layout reserves 4 rows of header and 3 of footer, so anything shorter
/// leaves no room for content and ratatui panics on the zero-height area.
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 12;

/// Rows consumed by the fixed header and footer blocks in [`draw`].
const HEADER_ROWS: u16 = 4;
const FOOTER_ROWS: u16 = 3;

// Compile-time: the minimum must leave at least one row for content. Editing any
// of these constants into an inconsistent state fails the build rather than
// panicking inside ratatui at runtime.
const _: () = assert!(MIN_HEIGHT > HEADER_ROWS + FOOTER_ROWS);

/// Run the interactive TUI installer.
///
/// Returns once the user exits (`q`, `Ctrl+C`, or after install completes).
/// The caller is responsible for ensuring `flake_dir` contains a `flake.nix`
/// and a `nix` binary exists on PATH.
///
/// Refuses to start when stdout is not a terminal, or when the terminal is too
/// small to draw into, and says why. Previously `enable_raw_mode()` was the first
/// call, so running under CI, a pipe, or cron failed with a bare
/// "Device not configured (os error 6)".
pub fn run(flake_dir: &std::path::Path) -> io::Result<()> {
    use std::io::IsTerminal;

    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "`synapse install` needs an interactive terminal.\n\
             Run it directly, or use the non-interactive commands instead:\n\
             \x20 synapse update --all   # install/update every managed package\n\
             \x20 synapse status         # what is currently installed",
        ));
    }

    // Check the size before entering raw mode, so a failure here does not leave
    // the terminal in a half-configured state.
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        if cols < MIN_WIDTH || rows < MIN_HEIGHT {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "terminal is {cols}x{rows}; `synapse install` needs at least \
                     {MIN_WIDTH}x{MIN_HEIGHT}.\n\
                     Resize the window, or use `synapse update --all` instead."
                ),
            ));
        }
    }

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = InstallerState::new();

    // Warn if Nix is missing before entering the loop — we still let the user
    // browse the selector, but "install" is gated on Nix being present.
    // Resolved absolutely so the same lookup works under a scheduler's minimal PATH.
    let nix_bin = nix::resolve_bin().unwrap_or_else(|| "nix".to_string());

    let result = event_loop(&mut terminal, &mut app, flake_dir, &nix_bin);

    // Restore terminal regardless of how we exit.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("TUI error: {e}");
    }

    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut InstallerState,
    flake_dir: &std::path::Path,
    nix_bin: &str,
) -> io::Result<()> {
    let tick = Duration::from_millis(200);

    loop {
        terminal.draw(|f| draw(f, app))?;

        if event::poll(tick)? {
            match event::read()? {
                // Ctrl+C — always abort immediately.
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => {
                    // Partial Nix store entries are collected by `nix-collect-garbage`.
                    return Ok(());
                }

                Event::Key(KeyEvent { code, .. }) => match app.step {
                    Step::Select => match code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                        KeyCode::Char(' ') => app.toggle_current(),
                        KeyCode::Enter => {
                            if !app.any_selected() {
                                // nothing to do
                            } else if !matches!(app.nix_status, nix::NixStatus::Supported(_)) {
                                // Cannot install without Nix; the header already shows the error.
                            } else {
                                app.step = Step::Installing;
                                run_installs(terminal, app, flake_dir, nix_bin)?;
                            }
                        }
                        _ => {}
                    },
                    Step::Done => match code {
                        KeyCode::Char('q') | KeyCode::Enter | KeyCode::Esc => return Ok(()),
                        _ => {}
                    },
                    Step::Installing => {} // keys ignored during install
                },
                _ => {}
            }
        }

        if app.step == Step::Done {
            // Stay in Done state until user presses q/Enter.
        }
    }
}

/// Run all selected installs, keeping the TUI responsive throughout.
///
/// Each package's build runs on a worker thread; this loop polls its channel
/// on a 200 ms tick and re-renders between polls, so the progress gauge
/// animates instead of freezing on a blocking `wait()`. Ctrl+C is still read
/// during the build and aborts the remaining queue.
fn run_installs(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut InstallerState,
    flake_dir: &std::path::Path,
    nix_bin: &str,
) -> io::Result<()> {
    let cfg_dir = state::config_dir();

    let _lock = match state::acquire(&cfg_dir) {
        Ok(l) => l,
        Err(state::LockError::Held { owner_pid }) => {
            // Mark all as failed and return to the Done view.
            let msg = format!("another synapse is running (PID {owner_pid})");
            for s in app.install_status.iter_mut() {
                *s = InstallStatus::Failed(msg.clone());
            }
            app.step = Step::Done;
            return Ok(());
        }
        Err(state::LockError::Io(e)) => {
            return Err(e);
        }
    };

    let tick = Duration::from_millis(200);

    for (i, pkg) in PACKAGES.iter().enumerate() {
        if !app.selected[i] {
            continue;
        }

        app.install_status[i] = InstallStatus::Running {
            started: Instant::now(),
        };

        let rx = spawn_install(pkg, flake_dir.to_path_buf(), nix_bin.to_string());

        // Poll for completion while continuing to render and read input.
        let outcome = loop {
            terminal.draw(|f| draw(f, app))?;

            match rx.try_recv() {
                Ok(result) => break Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Worker died without sending — treat as a build failure.
                    break Some(Err("build thread terminated unexpectedly".to_string()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            // Ctrl+C during a build: stop the queue. The in-flight Nix build is
            // left to finish or be killed with us; its partial store paths are
            // unreferenced and removed by `nix-collect-garbage`.
            if event::poll(tick)? {
                if let Event::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) = event::read()?
                {
                    break None;
                }
            }
        };

        let Some(result) = outcome else {
            // Aborted: leave this package's status as Running-turned-Failed so
            // the user sees which one was interrupted.
            app.install_status[i] = InstallStatus::Failed("aborted by user".to_string());
            app.step = Step::Done;
            return Ok(());
        };

        match result {
            Ok(version) => {
                app.install_status[i] = InstallStatus::Done;
                // Persist to state.json; a write failure must not abort the run.
                if let Ok(mut st) = state::read(&cfg_dir) {
                    st.set_package(pkg.name, &version);
                    let _ = state::write(&cfg_dir, &st);
                }
                let _ = crate::commands::log::append(&format!(
                    "{} installed {} {}",
                    now_secs(),
                    pkg.name,
                    version
                ));
            }
            Err(msg) => {
                app.install_status[i] = InstallStatus::Failed(msg);
            }
        }

        terminal.draw(|f| draw(f, app))?;
    }

    app.step = Step::Done;

    // Doctor stub: verify installed binaries.
    let bins: Vec<&str> = PACKAGES
        .iter()
        .enumerate()
        .filter(|(i, _)| app.selected[*i] && matches!(app.install_status[*i], InstallStatus::Done))
        .map(|(_, p)| p.name)
        .collect();
    let _ = doctor_check(&bins); // surfaced by `synapse doctor` (SYN-11)

    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_list_is_three() {
        assert_eq!(PACKAGES.len(), 3);
        let names: Vec<_> = PACKAGES.iter().map(|p| p.nix_attr).collect();
        assert!(names.contains(&"herdr"));
        assert!(names.contains(&"omp"));
        assert!(names.contains(&"skillshare"));
    }

    #[test]
    fn installer_state_defaults_all_selected() {
        let s = InstallerState::new();
        assert!(s.any_selected());
        assert!(s.selected.iter().all(|&x| x));
    }

    #[test]
    fn toggle_deselects_and_reselects() {
        let mut s = InstallerState::new();
        s.list_state.select(Some(0));
        s.toggle_current();
        assert!(!s.selected[0]);
        s.toggle_current();
        assert!(s.selected[0]);
    }

    #[test]
    fn move_wraps_around() {
        let mut s = InstallerState::new();
        s.list_state.select(Some(0));
        s.move_up(); // should wrap to last
        assert_eq!(s.list_state.selected(), Some(PACKAGES.len() - 1));
        s.move_down(); // wrap back to 0
        assert_eq!(s.list_state.selected(), Some(0));
    }

    #[test]
    fn any_selected_false_when_all_off() {
        let mut s = InstallerState::new();
        for b in s.selected.iter_mut() {
            *b = false;
        }
        assert!(!s.any_selected());
    }

    #[test]
    fn install_status_is_terminal() {
        assert!(!InstallStatus::Pending.is_terminal());
        assert!(InstallStatus::Done.is_terminal());
        assert!(InstallStatus::Failed("x".into()).is_terminal());
        assert!(!InstallStatus::Running {
            started: Instant::now()
        }
        .is_terminal());
    }

    #[test]
    fn doctor_check_returns_one_per_bin() {
        let result = doctor_check(&["true"]);
        assert_eq!(result.len(), 1);
        // `true` is present on all supported platforms.
        assert_eq!(result[0].0, "true");
        assert!(result[0].1, "`true` should be found");
    }

    /// Rendering the full TUI at the declared minimum must not panic.
    ///
    /// TestBackend drives the real draw path with no terminal, so this covers the
    /// layout arithmetic that a zero-height area would blow up on.
    #[test]
    fn draws_at_minimum_size_without_panicking() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = InstallerState::new();
        let backend = TestBackend::new(MIN_WIDTH, MIN_HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();

        // Every step must render: select, mid-install, and done.
        terminal.draw(|f| draw(f, &mut app)).expect("select step");

        app.step = Step::Installing;
        app.install_status[0] = InstallStatus::Running {
            started: Instant::now(),
        };
        terminal
            .draw(|f| draw(f, &mut app))
            .expect("installing step");

        app.step = Step::Done;
        app.install_status[0] = InstallStatus::Done;
        app.install_status[1] = InstallStatus::Failed("boom".into());
        terminal.draw(|f| draw(f, &mut app)).expect("done step");
    }

    /// `draw_progress` builds its layout constraints from the selection, and an
    /// empty constraint list is a panic — so zero selected must still render.
    #[test]
    fn draws_with_one_and_zero_packages_selected() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = InstallerState::new();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        for b in app.selected.iter_mut() {
            *b = false;
        }
        app.selected[0] = true;
        app.step = Step::Installing;
        terminal.draw(|f| draw(f, &mut app)).expect("one selected");

        for b in app.selected.iter_mut() {
            *b = false;
        }
        terminal.draw(|f| draw(f, &mut app)).expect("none selected");
    }
}

mod commands;
mod nix;
mod platform;
mod shell;
mod state;
#[cfg(test)]
mod test_utils;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "synapse")]
#[command(version, about = "AI harness installer and auto-updater", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show platform, Nix version, installed packages, and update status
    Status,
    /// Diagnose issues (missing PATH, broken symlinks, stale locks)
    Doctor,
    /// List all managed packages with versions and install timestamps
    List,
    /// Update a package or all packages
    Update {
        /// Package name to update; omit to update all
        package: Option<String>,
        /// Update all installed packages
        #[arg(long)]
        all: bool,
    },
    /// Show recent install and update history
    Log,
    /// Show Synapse version and build info
    Version,
    /// Interactive installer (TUI)
    Install,
    /// Configure PATH and shell completions
    SetupShell {
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Revert a package, or all packages, to the previous version
    Rollback {
        /// Package to roll back; omit to roll back everything
        package: Option<String>,
    },
    /// Remove packages from Synapse management
    Uninstall {
        /// Package to remove; omit with --all to remove everything
        package: Option<String>,
        /// Remove all managed packages
        #[arg(long)]
        all: bool,
    },
    /// Capture, restore, and inspect portable AI stack state
    #[command(subcommand)]
    Stack(StackCommands),
    /// Manage the auto-update scheduler
    #[command(subcommand)]
    AutoUpdate(AutoUpdateCommands),
}

#[derive(Subcommand)]
enum StackCommands {
    /// Capture OMP plugins/MCP and Skillshare's Git source
    Capture {
        /// Tracked output directory; defaults inside Skillshare's Git root
        #[arg(long, value_name = "DIR")]
        output: Option<PathBuf>,
    },
    /// Restore a captured stack
    Restore {
        /// Captured stack directory; defaults inside Skillshare's Git root
        #[arg(long, value_name = "DIR")]
        input: Option<PathBuf>,
        /// Skillshare Git remote to clone before restoring on a new machine
        #[arg(long, value_name = "URL")]
        remote: Option<String>,
        /// Skillshare Git scope used with --remote
        #[arg(long, default_value = "root", value_name = "SCOPE")]
        git_root: String,
        /// Confirm that plugins and MCP commands in the stack may execute code
        #[arg(long)]
        trust: bool,
        /// Replace conflicting MCP server definitions
        #[arg(long)]
        force: bool,
    },
    /// Inspect a captured stack and missing environment variables
    Status {
        /// Captured stack directory; defaults inside Skillshare's Git root
        #[arg(long, value_name = "DIR")]
        input: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AutoUpdateCommands {
    /// Install and register the scheduler (launchd/systemd/cron)
    Enable,
    /// Unload and remove the scheduler unit
    Disable,
    /// Open the auto-update config in $EDITOR
    Config,
    /// Run an update immediately (acquires the state lock)
    Now,
    /// Show scheduler registration status and config
    Status,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        None => {
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            Ok(())
        }
        Some(Commands::Status) => commands::status::run(),
        Some(Commands::Doctor) => commands::doctor::run(),
        Some(Commands::List) => commands::list::run(),
        Some(Commands::Update { package, all }) => commands::update::run(package.as_deref(), all),
        Some(Commands::Log) => commands::log::run(),
        Some(Commands::Version) => commands::version::run(),
        Some(Commands::Install) => tui::run(&commands::update::locate_flake_dir()),
        Some(Commands::SetupShell { dry_run }) => commands::setup::run(dry_run),
        Some(Commands::Rollback { package }) => commands::rollback::run(package.as_deref()),
        Some(Commands::Uninstall { package, all }) => {
            commands::uninstall::run(package.as_deref(), all)
        }
        Some(Commands::Stack(sub)) => match sub {
            StackCommands::Capture { output } => commands::stack::capture(output.as_deref()),
            StackCommands::Restore {
                input,
                remote,
                git_root,
                trust,
                force,
            } => commands::stack::restore(
                input.as_deref(),
                remote.as_deref(),
                &git_root,
                trust,
                force,
            ),
            StackCommands::Status { input } => commands::stack::status(input.as_deref()),
        },
        Some(Commands::AutoUpdate(sub)) => match sub {
            AutoUpdateCommands::Enable => commands::auto_update::enable(),
            AutoUpdateCommands::Disable => commands::auto_update::disable(),
            AutoUpdateCommands::Config => commands::auto_update::open_config(),
            AutoUpdateCommands::Now => commands::auto_update::run_now(),
            AutoUpdateCommands::Status => commands::auto_update::show_status(),
        },
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

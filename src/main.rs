mod commands;
mod nix;
mod platform;
mod shell;
mod state;
#[cfg(test)]
mod test_utils;
mod tui;

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
    /// Manage the auto-update scheduler
    #[command(subcommand)]
    AutoUpdate(AutoUpdateCommands),
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

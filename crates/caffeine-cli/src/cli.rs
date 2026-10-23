//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const AFTER_HELP: &str = "\
Exit codes:
  0 success            3 no active session (status)   4 unsupported on this platform
  1 failure            5 permission denied            6 conflicts with current state
  2 usage error        7 configuration error          8 plugin error   9 not found

Run `caffeine doctor` if something does not work as expected.";

/// Keep your computer awake from the terminal.
///
/// Caffeine prevents idle-triggered sleep using native operating-system
/// mechanisms. It never simulates input, never blocks an explicit sleep
/// request and never changes persistent power settings unless asked to.
#[derive(Debug, Parser)]
#[command(name = "caffeine", version, about, long_about, after_help = AFTER_HELP)]
#[command(propagate_version = true, max_term_width = 100)]
pub struct Cli {
    /// Open the interactive terminal configuration editor.
    #[arg(long = "config")]
    pub config_editor: bool,

    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Print machine-readable JSON on stdout (errors included).
    #[arg(long, global = true)]
    pub json: bool,

    /// Print only errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Increase log output on stderr (-v info, -vv debug, -vvv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// When to use colors.
    #[arg(long, global = true, value_name = "WHEN")]
    pub color: Option<ColorChoice>,

    /// Use this configuration file instead of the default location.
    #[arg(long, global = true, value_name = "PATH", env = "CAFFEINE_CONFIG")]
    pub config_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start keeping the system awake (in the background by default).
    Start(StartArgs),
    /// Stop the active session and release every assertion.
    Stop(StopArgs),
    /// Show the active session, remaining time, lid and power state.
    Status,
    /// Keep the system awake while a command runs, then exit with its status.
    Run(RunArgs),
    /// Inspect the lid state and lid-close policy, or change the policy.
    Lid(LidArgs),
    /// Report which power-management features this machine supports.
    #[command(alias = "caps")]
    Capabilities,
    /// Diagnose configuration, permissions and platform problems.
    Doctor,
    /// Inspect and modify the configuration file.
    Config(ConfigArgs),
    /// Generate shell completion scripts.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Internal: the detached session host started by `caffeine start`.
    #[command(name = "__host", hide = true)]
    Host,
}

#[derive(Debug, Clone, Args)]
pub struct SessionArgs {
    /// How long to stay awake, e.g. 30m, 2h, 1h30m, or "indefinite".
    #[arg(short, long, value_name = "DURATION")]
    pub duration: Option<String>,

    /// Also keep the display on.
    #[arg(long, conflicts_with = "no_display")]
    pub display: bool,

    /// Let the display sleep even if the configuration keeps it on.
    #[arg(long)]
    pub no_display: bool,

    /// Do not prevent idle system sleep (e.g. only keep the display on).
    #[arg(long)]
    pub no_idle: bool,

    /// Lid-close handling during the session: system, keep-awake-on-ac, keep-awake.
    #[arg(long, value_name = "MODE")]
    pub lid: Option<String>,

    /// Reason recorded with the OS assertion (shown by powercfg, pmset, systemd-inhibit).
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct StartArgs {
    #[command(flatten)]
    pub session: SessionArgs,

    /// Stay attached to the terminal until the session ends or Ctrl+C.
    #[arg(short, long)]
    pub foreground: bool,

    /// Replace a running session that has different settings.
    #[arg(long)]
    pub replace: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StopArgs {
    /// Terminate the session host if it does not stop in time.
    #[arg(long)]
    pub force: bool,

    /// Seconds to wait for the session host (default: session.stop_timeout_secs).
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u32>,
}

#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub session: SessionArgs,

    /// The program to run, followed by its arguments.
    #[arg(required = true, num_args = 1.., last = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct LidArgs {
    #[command(subcommand)]
    pub action: Option<LidAction>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum LidAction {
    /// Show lid state and lid-close policy (the default).
    Status,
    /// Persistently change the lid-close action (Windows). Reversible with `lid restore`.
    Set {
        /// Action while plugged in: do-nothing, sleep, hibernate, shut-down.
        #[arg(long, value_name = "ACTION")]
        ac: Option<String>,
        /// Action while on battery: do-nothing, sleep, hibernate, shut-down.
        #[arg(long, value_name = "ACTION")]
        battery: Option<String>,
        /// Do not ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
    /// Restore lid-close settings Caffeine changed (including after a crash).
    Restore {
        /// Do not ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigAction {
    /// Print the configuration file location.
    Path,
    /// Print the effective configuration (secrets redacted).
    Show,
    /// Print one setting.
    Get { key: String },
    /// Change one setting (validated, comments preserved, atomic write).
    Set { key: String, value: String },
    /// Reset one setting to its default.
    Unset { key: String },
    /// Open the file in $VISUAL / $EDITOR and validate it afterwards.
    Edit,
    /// Check the configuration file for errors and unknown keys.
    Validate,
    /// Write a commented default configuration if none exists.
    Init,
    /// Replace the configuration with defaults (the old file is backed up).
    Reset {
        /// Do not ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
    /// List every setting with its type and description.
    List,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_common_invocations() {
        let cli = Cli::try_parse_from(["caffeine", "start", "--duration", "2h"]).unwrap();
        match cli.command {
            Some(Command::Start(a)) => assert_eq!(a.session.duration.as_deref(), Some("2h")),
            other => panic!("unexpected {other:?}"),
        }
        let cli = Cli::try_parse_from(["caffeine", "--config"]).unwrap();
        assert!(cli.config_editor);
        let cli = Cli::try_parse_from(["caffeine", "run", "--", "cargo", "build", "--release"]).unwrap();
        match cli.command {
            Some(Command::Run(a)) => assert_eq!(a.command, ["cargo", "build", "--release"]),
            other => panic!("unexpected {other:?}"),
        }
        let cli = Cli::try_parse_from(["caffeine", "status", "--json"]).unwrap();
        assert!(cli.global.json);
    }
}

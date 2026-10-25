//! The `caffeine` command.

mod cli;
mod commands;
mod context;
mod logging;
mod output;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use caffeine_core::config::DefaultCommand;
use caffeine_core::{Error, Result};

use crate::cli::{Cli, Command};
use crate::context::Ctx;
use crate::output::Output;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // --help and --version are "errors" for clap but exit 0.
            if !e.use_stderr() {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            if json_requested() {
                Output::json_only().error(&usage_error(&e));
            } else {
                let _ = e.print();
            }
            return ExitCode::from(2);
        }
    };

    // The session host has its own logging and never prints to a terminal.
    if matches!(cli.command, Some(Command::Host)) {
        return commands::session::host_main();
    }

    let mut ctx = match Ctx::load(&cli.global) {
        Ok(ctx) => ctx,
        Err(e) => {
            Output::plain().error(&e);
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    let level = match cli.global.verbose {
        0 if cli.global.quiet => log::LevelFilter::Error,
        0 => ctx
            .config
            .config()
            .general
            .log_level
            .to_filter()
            .min(log::LevelFilter::Warn),
        1 => log::LevelFilter::Info,
        2 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    logging::init_stderr(level);

    match dispatch(&mut ctx, &cli) {
        Ok(code) => code,
        Err(e) => {
            ctx.out.error(&e);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn dispatch(ctx: &mut Ctx, cli: &Cli) -> Result<ExitCode> {
    if cli.config_editor && cli.command.is_some() {
        return Err(Error::usage(
            "--config opens the configuration editor and cannot be combined with a command",
        ));
    }
    if cli.config_editor {
        return commands::config::interactive(ctx);
    }
    let Some(command) = &cli.command else {
        return match ctx.config.config().general.default_command {
            DefaultCommand::Status => commands::session::status(ctx),
            DefaultCommand::Start => commands::session::start(
                ctx,
                &cli::StartArgs {
                    session: cli::SessionArgs {
                        duration: None,
                        display: false,
                        no_display: false,
                        no_idle: false,
                        lid: None,
                        reason: None,
                    },
                    foreground: false,
                    replace: false,
                },
            ),
            DefaultCommand::Help => {
                let _ = Cli::command().print_help();
                Ok(ExitCode::SUCCESS)
            }
        };
    };
    match command {
        Command::Start(args) => commands::session::start(ctx, args),
        Command::Stop(args) => commands::session::stop(ctx, args),
        Command::Status => commands::session::status(ctx),
        Command::Run(args) => commands::session::run(ctx, args),
        Command::Lid(args) => commands::lid::run(ctx, args.action.as_ref()),
        Command::Capabilities => commands::info::capabilities(ctx),
        Command::Doctor => {
            let checks = commands::plugin::doctor_checks(ctx);
            commands::info::doctor(ctx, checks)
        }
        Command::Config(args) => {
            let secrets = commands::plugin::secret_paths(ctx);
            commands::config::run(ctx, &args.action, &secrets)
        }
        Command::Plugin(args) => commands::plugin::run(ctx, &args.action),
        Command::External(argv) => commands::plugin::external(ctx, argv),
        Command::Completions { shell } => {
            // clap_complete panics on write errors, so render into memory
            // and treat a closed pipe (`caffeine completions bash | head`)
            // like the rest of the output does.
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "caffeine", &mut script);
            match std::io::Write::write_all(&mut std::io::stdout().lock(), &script) {
                Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                    Err(Error::io("could not write the completion script", e))
                }
                _ => Ok(ExitCode::SUCCESS),
            }
        }
        Command::Host => unreachable!("handled before context creation"),
    }
}

/// Whether `--json` appears among the global options. Used when clap could
/// not parse the command line, so the error can still be one JSON document.
fn json_requested() -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|a| a != "--")
        .any(|a| a == "--json")
}

/// Turn a clap parse error into the CLI's usage error: clap's message up to
/// the first blank line, on one line and without its `error: ` prefix, with
/// clap's `Usage:` line as the hint.
fn usage_error(e: &clap::Error) -> Error {
    let rendered = e.render().to_string();
    let message = rendered
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    let message = message.strip_prefix("error: ").unwrap_or(&message);
    let hint = rendered
        .lines()
        .find(|l| l.starts_with("Usage: "))
        .map_or("run `caffeine --help`".to_string(), str::to_string);
    Error::usage(message).with_hint(hint)
}

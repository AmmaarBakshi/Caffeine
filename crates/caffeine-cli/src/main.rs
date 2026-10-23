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
            let code = if e.use_stderr() { 2 } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
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
        0 => ctx.config.config().general.log_level.to_filter().min(log::LevelFilter::Warn),
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
        return Err(Error::usage("--config opens the configuration editor and cannot be combined with a command"));
    }
    if cli.config_editor {
        return Err(Error::unsupported("the interactive editor is not available in this build")
            .with_hint("use `caffeine config edit` or `caffeine config set`"));
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
        Command::Doctor => commands::info::doctor(ctx, Vec::new()),
        Command::Config(args) => commands::config::run(ctx, &args.action, &[]),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "caffeine", &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
        Command::Host => unreachable!("handled before context creation"),
    }
}

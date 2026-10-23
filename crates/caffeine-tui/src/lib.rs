//! `caffeine --config`: a nano-inspired configuration editor that runs
//! entirely in the terminal.
//!
//! The editor works on a [`caffeine_core::config::ConfigFile`], so it gets
//! the same guarantees as `caffeine config set`: every change is validated
//! before it is accepted, comments and unknown keys survive, saving is
//! atomic with a `.bak` copy, and a file changed by another program since
//! it was opened is never overwritten. Settings that need a capability the
//! machine lacks are shown as unsupported and cannot be changed.

pub mod app;
pub mod rows;
pub mod ui;

use std::io::IsTerminal;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{cursor, execute};

use caffeine_core::{Error, Result};

pub use app::{App, EditorInput, Outcome};

/// Restores the terminal when dropped, including during a panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<TerminalGuard> {
        enable_raw_mode().map_err(|e| Error::io("switching the terminal to raw mode", e))?;
        let guard = TerminalGuard;
        execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide)
            .map_err(|e| Error::io("entering the alternate screen", e))?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
}

/// Run the editor until the user exits.
pub fn run(input: EditorInput) -> Result<Outcome> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(
            Error::usage("the configuration editor needs an interactive terminal")
                .with_hint("use `caffeine config set <key> <value>` in scripts"),
        );
    }
    let mut app = App::new(input);

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous_hook(info);
    }));

    (|| -> Result<Outcome> {
        let _guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))
            .map_err(|e| Error::io("initializing the terminal", e))?;
        terminal
            .clear()
            .map_err(|e| Error::io("clearing the terminal", e))?;
        loop {
            terminal
                .draw(|f| ui::draw(f, &app))
                .map_err(|e| Error::io("drawing the editor", e))?;
            if let Some(outcome) = app.outcome.clone() {
                return Ok(outcome);
            }
            match event::read().map_err(|e| Error::io("reading the keyboard", e))? {
                // Windows also reports key releases; act on presses only.
                Event::Key(key) if key.kind != KeyEventKind::Release => app.handle(key),
                _ => {}
            }
        }
    })()
}

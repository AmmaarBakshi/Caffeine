//! Terminal output: human-readable or JSON, with optional color.
//!
//! In JSON mode each command prints exactly one JSON document on stdout,
//! including for errors (`{"error": {...}}`), and nothing else on stdout.
//! Warnings go to stderr in human mode and into the document in JSON mode.

use std::io::{IsTerminal, Write};

use serde::Serialize;

use caffeine_core::Error;
use caffeine_core::config::ColorMode;

use crate::cli::{ColorChoice, GlobalArgs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Bold,
    Dim,
    Good,
    Warn,
    Bad,
    Accent,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Style::Bold => "1",
            Style::Dim => "2",
            Style::Good => "32",
            Style::Warn => "33",
            Style::Bad => "31",
            Style::Accent => "36",
        }
    }
}

#[derive(Debug)]
pub struct Output {
    json: bool,
    quiet: bool,
    color: bool,
    color_err: bool,
    warnings: std::cell::RefCell<Vec<String>>,
}

impl Output {
    pub fn new(args: &GlobalArgs, config_color: ColorMode) -> Output {
        let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let wanted = |is_tty: bool| match args.color {
            Some(ColorChoice::Always) => true,
            Some(ColorChoice::Never) => false,
            Some(ColorChoice::Auto) | None => match config_color {
                ColorMode::Always => true,
                ColorMode::Never => false,
                ColorMode::Auto => {
                    is_tty && !no_color_env && std::env::var("TERM").map_or(true, |t| t != "dumb")
                }
            },
        };
        let color = !args.json && wanted(std::io::stdout().is_terminal());
        let color_err = wanted(std::io::stderr().is_terminal());
        if color || color_err {
            enable_ansi();
        }
        Output {
            json: args.json,
            quiet: args.quiet,
            color,
            color_err,
            warnings: Default::default(),
        }
    }

    /// An output for contexts where the CLI args are not available.
    pub fn plain() -> Output {
        Output {
            json: false,
            quiet: false,
            color: false,
            color_err: false,
            warnings: Default::default(),
        }
    }

    pub fn is_json(&self) -> bool {
        self.json
    }

    pub fn paint(&self, style: Style, text: impl AsRef<str>) -> String {
        paint(self.color, style, text.as_ref())
    }

    fn paint_err(&self, style: Style, text: &str) -> String {
        paint(self.color_err, style, text)
    }

    /// A normal line of human output (suppressed by --quiet and --json).
    pub fn line(&self, text: impl AsRef<str>) {
        if !self.json && !self.quiet {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", text.as_ref());
        }
    }

    /// Human output that is the *answer* to the command (e.g. `config get`)
    /// and is therefore printed even with --quiet.
    pub fn answer(&self, text: impl AsRef<str>) {
        if !self.json {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", text.as_ref());
        }
    }

    /// Key/value row with an aligned label.
    pub fn field(&self, label: &str, value: impl AsRef<str>) {
        self.line(format!(
            "  {:<14} {}",
            self.paint(Style::Dim, format!("{label}:")),
            value.as_ref()
        ));
    }

    /// A warning: stderr in human mode, collected for JSON mode.
    pub fn warn(&self, text: impl Into<String>) {
        let text = text.into();
        if self.json {
            self.warnings.borrow_mut().push(text);
        } else if !self.quiet {
            eprintln!("{} {text}", self.paint_err(Style::Warn, "warning:"));
        }
    }

    pub fn note(&self, text: impl AsRef<str>) {
        if !self.json && !self.quiet {
            eprintln!(
                "{} {}",
                self.paint_err(Style::Accent, "note:"),
                text.as_ref()
            );
        }
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }

    /// Emit the JSON document for a command. `warnings` collected so far are
    /// merged into a top-level `warnings` array when `value` is an object.
    pub fn json<T: Serialize>(&self, value: &T) {
        if !self.json {
            return;
        }
        let mut v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        let warnings = self.take_warnings();
        if let serde_json::Value::Object(map) = &mut v
            && !warnings.is_empty()
        {
            map.insert("warnings".into(), serde_json::json!(warnings));
        }
        let mut out = std::io::stdout().lock();
        let _ = serde_json::to_writer_pretty(&mut out, &v);
        let _ = writeln!(out);
    }

    /// Emit one compact JSON line. Used by long-running foreground commands,
    /// whose JSON output is a stream of events (JSON Lines).
    pub fn json_event<T: Serialize>(&self, value: &T) {
        if !self.json {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, value);
        let _ = writeln!(out);
        let _ = out.flush();
    }

    /// Report an error in the appropriate format.
    pub fn error(&self, err: &Error) {
        if self.json {
            self.json(&serde_json::json!({ "error": err.to_report() }));
            return;
        }
        eprintln!("{} {err}", self.paint_err(Style::Bad, "error:"));
        if let Some(hint) = err.hint() {
            eprintln!("  {} {hint}", self.paint_err(Style::Dim, "hint:"));
        }
    }
}

fn paint(enabled: bool, style: Style, text: &str) -> String {
    if enabled {
        format!("\x1b[{}m{text}\x1b[0m", style.code())
    } else {
        text.to_string()
    }
}

/// Ask a yes/no question on the terminal. Returns `None` when stdin is not
/// interactive, so callers can require an explicit `--yes` instead.
pub fn confirm(question: &str) -> Option<bool> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return None;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok()?;
    Some(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Enable ANSI escape processing on Windows consoles (no-op elsewhere).
fn enable_ansi() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetConsoleMode,
        };
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: querying and updating the mode of our own std handles.
            unsafe {
                let h = GetStdHandle(which);
                let mut mode = 0;
                if GetConsoleMode(h, &mut mode) != 0 {
                    SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
}

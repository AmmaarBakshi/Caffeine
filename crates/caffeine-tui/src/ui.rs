//! Drawing the editor.
//!
//! ```text
//!  Caffeine 0.1.0 configuration    /home/me/.config/caffeine/config.toml   Modified
//! ┌Sections─────────┐┌Power management──────────────────────────────────────┐
//! │ 1 General       ││ Idle-sleep prevention           on                    │
//! │ 2 Power         ││ Display-sleep policy            allow-sleep           │
//! │ ...             ││ Lid-close policy during sessions  unsupported         │
//! └─────────────────┘└───────────────────────────────────────────────────────┘
//! ┌power.display────────────────────────────────────────────────────────────┐
//! │ keep-on also prevents the display from sleeping during sessions.        │
//! └─────────────────────────────────────────────────────────────────────────┘
//!  [ status message ]
//! ^S Save  ^X Exit  ^G Help  Enter Edit  Space Toggle  ...
//! ```

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use caffeine_core::config::Section;
use caffeine_plugin_api::manifest::SettingType;
use caffeine_plugin_manager::TrustStatus;

use crate::app::{App, Mode, StatusKind};
use crate::rows::{Availability, Row, is_default, plugin_value, show_plugin_value};

const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 16;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let msg = Paragraph::new(format!(
            "The terminal is too small ({}x{}); the editor needs at least {MIN_WIDTH}x{MIN_HEIGHT}.\nPress ^X to exit.",
            area.width, area.height
        ))
        .wrap(Wrap { trim: true });
        f.render_widget(msg, area);
        return;
    }
    let [title, body, help, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(5),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(area);
    draw_title(f, app, title);
    let [sections, settings] =
        Layout::horizontal([Constraint::Length(22), Constraint::Min(30)]).areas(body);
    draw_sections(f, app, sections);
    draw_rows(f, app, settings);
    draw_help(f, app, help);
    draw_status(f, app, status);
    draw_keys(f, app, keys);
    match &app.mode {
        Mode::Pick { options, index } => draw_picker(f, options, *index, settings),
        Mode::ConfirmTrust { plugin } => draw_trust(f, app, *plugin, area),
        Mode::Help => draw_help_screen(f, area),
        _ => {}
    }
}

fn draw_title(f: &mut Frame, app: &App, area: Rect) {
    let flag = if app.config.read_only_reason().is_some() {
        "Read-only"
    } else if app.is_dirty() {
        "Modified"
    } else {
        ""
    };
    let left = format!(" Caffeine {} configuration", app.version);
    let path = app.config.path().display().to_string();
    let width = area.width as usize;
    let room = width.saturating_sub(left.len() + flag.len() + 4);
    let path = if path.chars().count() > room {
        let tail: String = path
            .chars()
            .rev()
            .take(room.saturating_sub(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        path
    };
    let pad = width.saturating_sub(left.len() + path.chars().count() + flag.len() + 2);
    let line = format!(
        "{left}{}{path}{}{flag} ",
        " ".repeat(pad / 2 + 1),
        " ".repeat(pad - pad / 2)
    );
    f.render_widget(Paragraph::new(line).style(Style::new().reversed()), area);
}

fn draw_sections(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = Section::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| ListItem::new(format!("{} {}", i + 1, short_title(*s))))
        .collect();
    let mut state = ListState::default().with_selected(Some(app.section));
    let list = List::new(items)
        .block(Block::new().borders(Borders::ALL).title("Sections"))
        .highlight_style(Style::new().bold().reversed());
    f.render_stateful_widget(list, area, &mut state);
}

fn short_title(s: Section) -> &'static str {
    match s {
        Section::Power => "Power",
        other => other.title(),
    }
}

/// Label, value and style of one row.
fn row_text(app: &App, row: &Row) -> (String, String, Style) {
    match row {
        Row::Setting { def, availability } => {
            let value = app.config.get(def.key).unwrap_or_default();
            match availability {
                Availability::Unsupported(_) => (
                    def.label.to_string(),
                    "unsupported on this machine".into(),
                    Style::new().dim(),
                ),
                Availability::Supported => {
                    let shown = match value.as_str() {
                        "true" => "on".to_string(),
                        "false" => "off".to_string(),
                        "" => "(default)".to_string(),
                        _ => value.clone(),
                    };
                    let style = if is_default(&app.config, &app.defaults, def.key) {
                        Style::new()
                    } else {
                        Style::new().bold()
                    };
                    (def.label.to_string(), shown, style)
                }
            }
        }
        Row::Plugin(i) => {
            let p = &app.plugins[*i];
            let on = app.is_enabled(&p.name);
            let mut flags = vec![p.origin.as_str().to_string()];
            if !p.errors.is_empty() {
                flags.push("unusable".into());
            } else if on
                && matches!(
                    app.trust.get(*i),
                    Some(TrustStatus::Changed | TrustStatus::NotTrusted)
                )
            {
                flags.push("needs review: toggle off and on".into());
            }
            let style = if p.errors.is_empty() {
                Style::new()
            } else {
                Style::new().dim()
            };
            (
                format!("Plugin {} {}", p.name, p.version),
                format!(
                    "[{}] {}  ({})",
                    if on { "x" } else { " " },
                    if on { "enabled" } else { "disabled" },
                    flags.join(", ")
                ),
                style.bold(),
            )
        }
        Row::PluginSetting { plugin, spec } => {
            let name = &app.plugins[*plugin].name;
            let value = plugin_value(&app.config, name, spec);
            let mut shown = show_plugin_value(spec, value.as_ref());
            if spec.kind == SettingType::Bool {
                shown = match shown.as_str() {
                    "true" => "on".into(),
                    "false" => "off".into(),
                    other => other.into(),
                };
            }
            (format!("  {}", spec.key), shown, Style::new())
        }
        Row::Info { label, value } => (label.clone(), value.clone(), Style::new().italic()),
    }
}

fn draw_rows(f: &mut Frame, app: &App, area: Rect) {
    let label_width = app
        .rows
        .iter()
        .map(|r| row_text(app, r).0.chars().count())
        .max()
        .unwrap_or(10)
        .min(area.width as usize / 2);
    let mut items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|r| {
            let (label, value, style) = row_text(app, r);
            ListItem::new(Line::from(vec![
                Span::raw(format!(" {label:<label_width$}  ")),
                Span::styled(value, style),
            ]))
        })
        .collect();
    if app.section() == Section::Plugins
        && let Some(err) = app.plugin_error()
    {
        items.push(ListItem::new(
            Line::from(format!(" plugins unavailable: {err}")).dim(),
        ));
    }
    let mut state = ListState::default().with_selected(Some(app.row));
    let list = List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .title(app.section().title()),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let (title, lines): (String, Vec<Line>) = match app.current() {
        Some(Row::Setting { def, availability }) => {
            let mut lines = vec![Line::from(def.help.to_string())];
            lines.push(Line::from(format!("Accepts: {}", def.kind.describe())).dim());
            if let Availability::Unsupported(why) = availability {
                lines.insert(0, Line::from(format!("Unsupported: {why}")).bold());
            }
            (def.key.to_string(), lines)
        }
        Some(Row::Plugin(i)) => {
            let p = &app.plugins[*i];
            let mut lines = vec![Line::from(p.description.clone())];
            let perms = if p.permissions.is_empty() {
                "none".to_string()
            } else {
                p.permissions
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            lines.push(Line::from(format!("Permissions: {perms}")).dim());
            if !p.platforms.is_empty() {
                lines.push(Line::from(format!("Platforms: {}", p.platforms.join(", "))).dim());
            }
            for e in &p.errors {
                lines.push(Line::from(format!("Problem: {e}")).bold());
            }
            for w in &p.warnings {
                lines.push(Line::from(format!("Warning: {w}")));
            }
            (format!("plugin {}", p.name), lines)
        }
        Some(Row::PluginSetting { plugin, spec }) => {
            let name = &app.plugins[*plugin].name;
            let mut lines = vec![Line::from(spec.description.clone())];
            lines.push(Line::from(format!("Accepts: {}", spec.describe_type())).dim());
            if spec.secret {
                lines.push(Line::from("Secret: hidden in `caffeine config show`").dim());
            }
            (format!("plugins.settings.{name}.{}", spec.key), lines)
        }
        Some(Row::Info { label, value }) => (label.clone(), vec![Line::from(value.clone())]),
        None => ("".into(), vec![]),
    };
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::new().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::Edit {
            buffer,
            cursor,
            masked,
        } => {
            let label = match app.current() {
                Some(Row::Setting { def, .. }) => def.label.to_string(),
                Some(Row::PluginSetting { spec, .. }) => spec.key.clone(),
                _ => String::new(),
            };
            let shown: String = if *masked {
                "*".repeat(buffer.chars().count())
            } else {
                buffer.clone()
            };
            let prompt = format!("{label}: ");
            let x = area.x + (prompt.chars().count() + cursor) as u16;
            if x < area.x + area.width {
                f.set_cursor_position((x, area.y));
            }
            Line::from(vec![Span::raw(prompt).bold(), Span::raw(shown)])
        }
        Mode::ConfirmExit => {
            Line::from(" Save modified configuration?  Y Yes   N No   C Cancel ").reversed()
        }
        _ => match &app.status {
            Some((StatusKind::Error, msg)) => Line::from(format!(" [ {msg} ] ")).bold().reversed(),
            Some((StatusKind::Info, msg)) => Line::from(format!(" [ {msg} ] ")),
            None => Line::from(""),
        },
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let pairs: &[&[(&str, &str)]] = match app.mode {
        Mode::Edit { .. } => &[
            &[("Enter", "Apply"), ("Esc", "Cancel"), ("^U", "Clear")],
            &[("←/→", "Move"), ("Home/End", "Jump")],
        ],
        Mode::Pick { .. } => &[
            &[("↑/↓", "Choose"), ("Enter", "Apply")],
            &[("Esc", "Cancel")],
        ],
        Mode::ConfirmExit | Mode::ConfirmTrust { .. } => {
            &[&[("Y", "Yes"), ("N", "No")], &[("Esc", "Cancel")]]
        }
        _ => &[
            &[
                ("^S", "Save"),
                ("^X", "Exit"),
                ("^G", "Help"),
                ("Tab", "Section"),
                ("1-5", "Jump"),
            ],
            &[
                ("Enter", "Edit"),
                ("Space", "Toggle"),
                ("←/→", "Cycle"),
                ("R", "Reset"),
                ("↑/↓", "Move"),
            ],
        ],
    };
    let lines: Vec<Line> = pairs
        .iter()
        .map(|row| {
            Line::from(
                row.iter()
                    .flat_map(|(k, v)| {
                        [
                            Span::raw(format!("{k:>5}")).reversed(),
                            Span::raw(format!(" {v:<10}")),
                        ]
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_picker(f: &mut Frame, options: &[String], index: usize, area: Rect) {
    let width = options.iter().map(|o| o.len()).max().unwrap_or(10) as u16 + 6;
    let rect = centered(area, width.max(20), options.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let mut state = ListState::default().with_selected(Some(index));
    let list = List::new(options.iter().map(|o| ListItem::new(format!(" {o}"))))
        .block(Block::new().borders(Borders::ALL).title("Choose"))
        .highlight_style(Style::new().reversed());
    f.render_stateful_widget(list, rect, &mut state);
}

fn draw_trust(f: &mut Frame, app: &App, plugin: usize, area: Rect) {
    let p = &app.plugins[plugin];
    let mut lines = vec![
        Line::from(format!("{} {} is a third-party plugin.", p.name, p.version)).bold(),
        Line::from(""),
        Line::from("It runs as a separate program with your privileges;"),
        Line::from("Caffeine cannot sandbox it. It declares:"),
    ];
    if p.permissions.is_empty() {
        lines.push(Line::from("  (no permissions)"));
    }
    for perm in &p.permissions {
        lines.push(Line::from(format!(
            "  - {}: {}",
            perm.as_str(),
            perm.description()
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Trust these exact files and enable it on save? (y/n)").bold());
    let rect = centered(area, 66, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::new().borders(Borders::ALL).title("Enable plugin")),
        rect,
    );
}

fn draw_help_screen(f: &mut Frame, area: Rect) {
    let text = [
        "Caffeine configuration editor",
        "",
        "  ↑ ↓  j k      move between settings",
        "  Tab  S-Tab    next / previous section (or 1-5)",
        "  Enter         edit the value (toggle, choose or type)",
        "  Space ← →     toggle, or cycle through choices",
        "  R  Delete     reset the setting to its default",
        "  ^S            save (atomic; a .bak copy is kept)",
        "  ^X  q         exit (asks about unsaved changes)",
        "",
        "Bold values differ from the default. Settings this machine cannot",
        "honor are marked unsupported and cannot be changed. Comments and",
        "unknown keys in the file are preserved. If another program changed",
        "the file since it was opened, saving is refused.",
        "",
        "Press any key to return.",
    ];
    let rect = centered(area, 72, text.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(text.iter().map(|l| Line::from(*l)).collect::<Vec<_>>())
            .block(Block::new().borders(Borders::ALL).title("Help")),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app_with, ctrl, key};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_sections_settings_and_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with(&tmp.path().join("c.toml"), None);
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Caffeine 0.0.0 configuration"));
        assert!(screen.contains("Startup behavior"));
        assert!(screen.contains("^S"));
        assert!(screen.contains("general.default_command"));

        app.handle(key(KeyCode::Char('2')));
        let screen = render(&app, 100, 30);
        assert!(screen.contains("unsupported on this machine"), "{screen}");

        app.handle(key(KeyCode::Char('1')));
        app.handle(key(KeyCode::Down));
        app.handle(key(KeyCode::Enter));
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Choose"));

        app.handle(key(KeyCode::Esc));
        app.handle(ctrl('g'));
        assert!(render(&app, 100, 30).contains("Press any key to return"));
    }

    #[test]
    fn tiny_terminals_get_a_message() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(&tmp.path().join("c.toml"), None);
        assert!(render(&app, 40, 10).contains("too small"));
    }
}

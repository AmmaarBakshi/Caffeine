# Configuration reference

Caffeine reads one TOML file. Every setting has a validated default, so a
missing or empty file is a valid configuration. Edit it with
`caffeine --config` (interactive), `caffeine config set KEY VALUE`,
`caffeine config edit`, or any text editor.

## Location

`caffeine config path` prints the file in use.

| Purpose | Linux | macOS | Windows |
|---|---|---|---|
| Configuration | `$XDG_CONFIG_HOME/caffeine/config.toml` (`~/.config/...`) | `~/Library/Application Support/caffeine/config.toml` | `%APPDATA%\caffeine\config.toml` |
| Durable data (trust records, restore journal, last session, installed plugins) | `$XDG_DATA_HOME/caffeine` | `~/Library/Application Support/caffeine` | `%LOCALAPPDATA%\caffeine` |
| Runtime state (live session, locks) | `$XDG_RUNTIME_DIR/caffeine` | `<data>/run` | `<data>\run` |
| Logs | `$XDG_STATE_HOME/caffeine` | `~/Library/Logs/caffeine` | `<data>\logs` |

`CAFFEINE_CONFIG` (or `--config-file`) selects a different configuration file.
`CAFFEINE_HOME` puts everything under one directory.

Runtime state is kept apart from configuration on purpose. A running session
is never written into the configuration file, and a reboot clears it. A
persistent power-policy change made with `caffeine lid set` is recorded in the
durable data directory, so it can always be undone.

## Guarantees

- **Validation first.** A value is checked against its type and the rules
  across settings (for example, `default_duration` can't exceed
  `max_duration`) before anything is written. A rejected edit changes nothing.
- **Your formatting is kept.** Comments, ordering, whitespace and unknown keys
  survive edits made with `config set`, `config unset` and the interactive
  editor.
- **Atomic writes.** The new file is written next to the old one, flushed and
  renamed over it. The previous version is kept as `config.toml.bak`.
- **No silent overwrites.** If another program changed the file after Caffeine
  read it, saving fails with exit 6 and nothing is written.
- **Broken files are left alone.** If the file isn't valid TOML or has invalid
  values, commands that only read settings warn and fall back to defaults.
  Commands that would write the file refuse until you fix it
  (`caffeine config edit`) or reset it (`caffeine config reset`, which keeps a
  timestamped backup).
- **Upgrades keep your settings.** The file carries a `schema_version`. Older
  files are migrated in memory and rewritten only when you save. A file written
  by a *newer* Caffeine is read but never modified.
- **Unknown keys are kept.** They produce a warning (a typo is the usual
  cause) but are left in place, so a file shared with a newer version still
  works.

## Settings

Values below are the defaults. `caffeine config list` prints the same table
with your current values.

### General

| Key | Default | Accepts | Description |
|---|---|---|---|
| `general.default_command` | `status` | status \| help \| start | What a bare `caffeine` invocation does: show status, print help, or start a session. |
| `general.log_level` | `warn` | error \| warn \| info \| debug \| trace | Minimum level written to the session log and to stderr with -v. |
| `general.notifications` | `none` | none \| bell \| osc9 | Terminal-only notice when a foreground session ends: none, the terminal bell, or an OSC 9 desktop notification escape (supported by some terminals). |
| `general.color` | `auto` | auto \| always \| never | Colorize human-readable output. NO_COLOR in the environment always disables color. |
| `general.safety_warnings` | `true` | true \| false | Warn about battery drain, ventilation and closed-lid heat when starting sessions. |

### Power management

| Key | Default | Accepts | Description |
|---|---|---|---|
| `power.prevent_idle_sleep` | `true` | true \| false | Sessions stop the idle timer from suspending the system. Explicit suspend is never blocked. |
| `power.display` | `allow-sleep` | allow-sleep \| keep-on | keep-on also prevents the display from sleeping during sessions. |
| `power.lid_detection` | `true` | true \| false | Report whether the lid is open or closed in `caffeine status`. |
| `power.lid_session` | `system` | system \| keep-awake-on-ac \| keep-awake | Keep running with the lid closed while a session is active. Mind ventilation: a closed laptop in a bag can overheat. |
| `power.on_battery` | `continue` | continue \| stop | Continue the session or end it when the machine starts running on battery. |
| `power.battery_threshold` | `0` | integer 0..=100 | End the session when battery charge falls below this percentage. 0 disables the check. |

### Sessions

| Key | Default | Accepts | Description |
|---|---|---|---|
| `session.default_duration` | `indefinite` | duration (e.g. 30m, 2h, 1h30m) or indefinite | Used by `caffeine start` without --duration. |
| `session.max_duration` | `indefinite` | duration (e.g. 30m, 2h, 1h30m) or indefinite | Upper bound applied to every session, including indefinite ones. |
| `session.on_expire` | `stop` | stop \| stop-and-notify | stop-and-notify also sends the configured terminal notification (foreground sessions). |
| `session.on_conflict` | `error` | error \| keep \| replace | When `start` asks for different settings than the running session: fail, keep the running session, or replace it. |
| `session.stop_timeout_secs` | `10` | integer 1..=300 | How long `caffeine stop` waits for the session host to release its assertions. |
| `session.record_last_session` | `true` | true \| false | Keep a record of how the previous session ended, shown by `caffeine status`. |

### Plugins

| Key | Default | Accepts | Description |
|---|---|---|---|
| `plugins.enabled` | `(unset)` | comma-separated list | Use `caffeine plugin enable <name>` so permissions are reviewed and trust is recorded. |
| `plugins.search_paths` | `(unset)` | comma-separated absolute paths | Additional directories searched for third-party plugins. |

### Advanced

| Key | Default | Accepts | Description |
|---|---|---|---|
| `advanced.backend` | `auto` | auto \| power-request \| execution-state | Native mechanism used for sessions. `auto` picks the best available. |
| `advanced.verbose_diagnostics` | `false` | true \| false | Include low-level details in `caffeine doctor`. |
| `advanced.log_file` | `(unset)` | absolute path, or empty for the default | Where background sessions write their log. Empty uses the default location. |
| `advanced.experimental` | `(unset)` | comma-separated subset of: session-lid-override | session-lid-override: on Windows, temporarily change the lid-close action while a session runs (restored on stop; `caffeine lid restore` recovers after a crash). |

`advanced.backend` accepts different values on each platform:

| Platform | Values |
|---|---|
| Windows | `auto`, `power-request` (`PowerSetRequest`, visible in `powercfg /requests`), `execution-state` (`SetThreadExecutionState`) |
| macOS | `auto`, `iokit` |
| Linux | `auto`, `logind`, `desktop`, `logind+desktop` |

Settings that need a capability this machine lacks (see
`caffeine capabilities`) can still be written to the file, so the file stays
portable. They have no effect, the interactive editor shows them as
unsupported, and `caffeine start` refuses to start a session that depends on
them.

## Plugin settings

Each plugin declares its settings in its manifest. Values go in a table named
after the plugin:

```toml
[plugins]
enabled = ["build"]

[plugins.settings.build]
default_task = "release"

[plugins.settings.build.tasks.release]
program = "cargo"
args = ["build", "--release"]
```

Values are validated against the plugin's schema. An invalid value stops the
plugin from running and is reported by `caffeine plugin info` and
`caffeine doctor`. Settings a plugin marks as secret are shown as
`<redacted>` by `config show` and `plugin info`, and never appear in logs.
`config show` also redacts any plugin setting whose name contains `token`,
`secret`, `password` or a similar word.

Loading the configuration never runs plugin code. A plugin runs only when you
invoke one of its commands, or when `doctor` or `plugin info` asks an enabled
plugin for its health checks.

## Interactive editor

`caffeine --config` opens a full-screen editor in the terminal. It feels
familiar if you use nano: the shortcuts are listed at the bottom, `^S` saves
and `^X` exits.

```
 Caffeine 0.1.0 configuration      ~/.config/caffeine/config.toml      Modified
┌Sections────────────┐┌Power management──────────────────────────────────────┐
│1 General           ││ Idle-sleep prevention             on                  │
│2 Power             ││ Display-sleep policy              allow-sleep         │
│3 Sessions          ││ Lid-state detection               on                  │
│4 Plugins           ││ Lid-close policy during sessions  unsupported on this │
│5 Advanced          ││ On switch to battery              continue            │
└────────────────────┘└───────────────────────────────────────────────────────┘
┌power.lid_session──────────────────────────────────────────────────────────────┐
│Unsupported: lid override for the session is unsupported here: ...             │
└───────────────────────────────────────────────────────────────────────────────┘
   ^S Save         ^X Exit         ^G Help        Tab Section     1-5 Jump
Enter Edit      Space Toggle      ←/→ Cycle         R Reset       ↑/↓ Move
```

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Move between settings |
| `Tab` / `Shift+Tab`, `1`–`5` | Switch section |
| `Enter` | Toggle a switch, choose from a list, or type a value |
| `Space`, `←` `→` | Toggle or cycle through choices |
| `R` / `Delete` | Reset to the default |
| `^S` | Save |
| `^X` / `q` | Exit (asks first if there are unsaved changes) |
| `^G` / `F1` / `?` | Help |

When typing a value, `Enter` applies it, `Esc` cancels and `^U` clears the
line. A rejected value keeps the prompt open and shows the reason. Values that
differ from the default are shown in bold.

The **Plugins** section lists every plugin with an on/off switch and its
settings. Switching on a third-party plugin first shows the permissions it
declares; trust is recorded only when you save. Tables, such as the build
plugin's `tasks`, are edited in the file (`caffeine config edit`). The
**Advanced** section shows the configuration, data, runtime, log and plugin
paths.

The editor needs an interactive terminal. In scripts, use `caffeine config set`.

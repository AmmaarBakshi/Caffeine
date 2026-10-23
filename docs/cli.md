# CLI reference

```
caffeine [GLOBAL OPTIONS] <COMMAND> [ARGS]
caffeine --config
```

Running `caffeine` with no command does what `general.default_command` says:
`status` (the default), `help` or `start`.

## Global options

| Option | Meaning |
|---|---|
| `--json` | Print exactly one JSON document on stdout, including for errors (`{"error": {...}}`). Warnings go into a top-level `warnings` array instead of stderr. Long-running foreground commands print JSON Lines events instead (see below). |
| `-q, --quiet` | Print only errors. Answers to questions such as `config get` are still printed. |
| `-v, --verbose` | More log output on stderr: `-v` info, `-vv` debug, `-vvv` trace. |
| `--color auto\|always\|never` | Colored output. `NO_COLOR` disables color in `auto` mode. |
| `--config-file PATH` | Use this configuration file (also `CAFFEINE_CONFIG`). |
| `--config` | Open the interactive configuration editor. Can't be combined with a command. |
| `-h, --help`, `-V, --version` | Help and version. |

Global options go **before** a plugin command (`caffeine --json build`).
Everything after a plugin command's name belongs to the plugin.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Failure (an operation failed, or a check failed in `doctor`) |
| 2 | Usage error (bad arguments or values, missing confirmation) |
| 3 | `status`: no session is active |
| 4 | Not supported on this platform or machine |
| 5 | Permission denied |
| 6 | Conflicts with the current state (a different session is running, the file changed on disk, ...) |
| 7 | Configuration error (the file can't be read or is invalid) |
| 8 | Plugin error (not enabled, not trusted, failed) |
| 9 | Not found |

`caffeine run` and plugin commands exit with the exit code of the program or
plugin command they ran.

## Sessions

### `caffeine start`

Start a session in the background and return as soon as the operating system
has granted the assertion. A detached *session host* process holds the
assertion until the session ends.

| Option | Meaning |
|---|---|
| `-d, --duration D` | `30m`, `2h`, `1h30m`, `90s`, `1d` or `indefinite`. Default: `session.default_duration`. Limited to `session.max_duration`. |
| `--display` / `--no-display` | Also keep the display on, or don't, overriding `power.display`. |
| `--no-idle` | Don't block idle sleep (for example, only keep the display on). |
| `--lid MODE` | `system` (default), `keep-awake-on-ac` or `keep-awake`. Needs the `lid-session-override` capability. |
| `--reason TEXT` | Text attached to the OS assertion. It shows up in `powercfg /requests`, `pmset -g assertions` and `systemd-inhibit --list`. |
| `-f, --foreground` | Stay attached until the session ends or Ctrl+C. |
| `--replace` | Replace a running session that has different settings. |

Starting is idempotent. Repeating the same request reports `already-active`. A
request with different settings is handled by `session.on_conflict`: `error`
(exit 6, the default), `keep`, or `replace`.

Before acquiring anything, `start` checks the capability report. If a
requested capability isn't available it fails with exit 4 (or 5 when
permission is the problem) and never reports a session it doesn't hold. When a
capability is only partly supported (for example on Modern Standby PCs), the
session starts and a warning explains the limitation.

JSON: `{"result": "started"|"already-active"|"kept"|"replaced", "session": {...}}`.
With `--foreground --json`, the output is JSON Lines:
`{"event":"started","session":{...}}`, then `{"event":"ended","record":{...}}`.

### `caffeine stop`

Ask the session host to release its assertions and exit, then wait up to
`--timeout` seconds (default `session.stop_timeout_secs`). `--force`
terminates a host that doesn't respond. Stopping when nothing runs succeeds,
and also removes stale state left behind by a host that was killed.

JSON: `{"stopped": bool, "session_id": "...", "detail": "...", "record": {...}}`.

### `caffeine status`

Show the active session (mode, start time, remaining time, the assertions
held, battery rule, suspected resumes from sleep, host PID), the lid state,
the power source, and any lid-policy change that still needs restoring. Exit
code 0 if a session is active, 3 if not.

JSON fields: `state` (`active`, `starting`, `stale`, `inactive`), `session`,
`remaining_secs`, `last_session`, `lid`, `power`, `recovery`.

### `caffeine run [SESSION OPTIONS] -- COMMAND [ARGS]...`

Keep the system awake while `COMMAND` runs, then exit with its exit code. The
program runs directly, without a shell. If a running session already covers
the request, the command just runs. If the session ends early (because of
`caffeine stop`, the battery rule or the duration), Caffeine keeps waiting for
the command and doesn't kill it.

## Lid

### `caffeine lid` / `caffeine lid status`

The lid state (open or closed) and the configured lid-close action on AC power
and on battery, where the platform exposes them, together with whatever
currently overrides the policy (for example logind inhibitors). Exit 4 if the
machine has neither a lid nor a readable policy.

### `caffeine lid set [--ac ACTION] [--battery ACTION] [-y]`

**Windows only.** Persistently change the lid-close action in the active power
scheme. `ACTION` is `do-nothing`, `sleep`, `hibernate` or `shut-down`. The
command asks for confirmation; without a terminal it requires `--yes`. The
previous values are journaled first. Choosing `do-nothing` prints a heat
warning.

### `caffeine lid restore [-y]`

Restore every lid-close setting Caffeine changed, whether with `lid set` or by
a session that ended without cleaning up.

## Information

### `caffeine capabilities` (alias `caps`)

Every capability with its support level (`supported`, `limited`,
`requires-permission` or `unsupported`), the native mechanism, and an
explanation. The capabilities are `idle-sleep`, `display-sleep`, `lid-state`,
`lid-policy-read`, `lid-policy-write`, `lid-session-override`, `power-source`
and `resume-detection`.

### `caffeine doctor`

Check the configuration file, the runtime and data directories, session state,
the native backend (on Windows and macOS it acquires and releases a test
assertion; on Linux it checks logind and the D-Bus buses), permissions,
pending restores, and every enabled plugin, including the plugin's own health
checks. Exit 1 if any check fails. Set `advanced.verbose_diagnostics`, or pass
`-v`, to include informational checks and per-capability details.

## Configuration

| Command | Purpose |
|---|---|
| `caffeine --config` | Interactive editor (see [configuration.md](configuration.md#interactive-editor)). |
| `caffeine config path` | Print the configuration file location (`--json` lists every directory). |
| `caffeine config show` | Effective configuration as TOML, with secret plugin settings redacted. |
| `caffeine config get KEY` | One value. |
| `caffeine config set KEY VALUE` | Validate and write one value. Comments and unknown keys are preserved, the write is atomic, and a `.bak` copy is kept. |
| `caffeine config unset KEY` | Remove a value so the default applies. |
| `caffeine config edit` | Edit in `$VISUAL`/`$EDITOR` (or nano/vim/vi, or Notepad on Windows). The result is validated before it's saved. If it's invalid you can re-open it; nothing is written in the meantime. |
| `caffeine config validate` | Report errors and unknown keys (exit 7 if invalid). |
| `caffeine config init` | Write a commented default file if none exists. |
| `caffeine config reset [-y]` | Replace the file with defaults, keeping a timestamped backup. |
| `caffeine config list` | Every setting with its current value and accepted values. |

Lists are given comma-separated: `caffeine config set plugins.search_paths "/a,/b"`.

## Plugins

| Command | Purpose |
|---|---|
| `caffeine plugin list` | Built-in, installed and external plugins, with their state: `enabled`, `disabled`, `untrusted`, `changed` or `unusable`. |
| `caffeine plugin info NAME` | Manifest, permissions (and whether each is enforced), commands, settings, problems, and health checks if enabled. |
| `caffeine plugin enable NAME [-y]` | For a third-party plugin: show its permissions, ask for confirmation, record trust in its current files, then enable it. |
| `caffeine plugin disable NAME` | Disable it and forget its trust record. |
| `caffeine plugin install PATH [--upgrade]` | Copy a plugin into the plugin directory. Nothing is run and nothing is enabled. |
| `caffeine plugin remove NAME [-y]` | Delete an installed plugin. |
| `caffeine plugin create NAME [--path DIR] [--api-path DIR]` | Generate a starter project. |
| `caffeine plugin validate [PATH]` | Check a manifest, its executable and compatibility (exit 8 if invalid). |
| `caffeine plugin run NAME [-c COMMAND] [-- ARGS]` | Run a plugin command explicitly. |
| `caffeine COMMAND [ARGS]` | Run a command provided by an enabled plugin. |

Plugin commands marked *keeps the system awake* run inside a foreground
session, like `caffeine run`. With `--json` a plugin command prints
`{"plugin", "command", "exit_code", "data", "session"}`.

### Built-in `build` plugin

Once enabled with `caffeine plugin enable build`:

- `caffeine build-list` shows the configured tasks and the build systems
  detected in the current directory.
- `caffeine build [TASK] [ARGS...]` runs a task while keeping the system
  awake. Any `ARGS` are appended to the task's arguments.

See [plugins.md](plugins.md#the-built-in-build-plugin) for task configuration
and the exact detection rules.

## Shell completion

```sh
caffeine completions bash > ~/.local/share/bash-completion/completions/caffeine
caffeine completions zsh  > "${fpath[1]}/_caffeine"
caffeine completions fish > ~/.config/fish/completions/caffeine.fish
caffeine completions powershell >> $PROFILE
```

`elvish` is supported too.

## Environment variables

| Variable | Effect |
|---|---|
| `CAFFEINE_HOME` | Put configuration, data, runtime state and logs under one directory (`config/`, `data/`, `run/`, `logs/`). Useful for tests and portable setups. |
| `CAFFEINE_CONFIG` | Path of the configuration file. |
| `NO_COLOR` | Disable color in `auto` mode. |
| `VISUAL`, `EDITOR` | Editor for `caffeine config edit`. Split into words without a shell; quotes are honored. |

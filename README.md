# Caffeine

Keep your computer awake from the terminal, on Windows, macOS and Linux.

```console
$ caffeine start --duration 2h
Awake for 2h (until 2026-10-23T16:07:55Z)
  idle-sleep via PowerSetRequest(SystemRequired)

$ caffeine run -- cargo build --release     # awake only while the build runs
$ caffeine status
$ caffeine stop
```

Caffeine is a small, terminal-only utility written in Rust. It asks the
operating system not to put the machine to sleep while it is idle. It uses each
platform's documented mechanism for this: Windows power requests, macOS IOKit
power assertions, and systemd-logind/elogind or desktop-session inhibitors on
Linux. There is no GUI and no tray icon, and you don't need a desktop
environment.

## What it does, and what it won't do

- **Separate controls.** Idle-sleep prevention, keeping the display on, the
  lid-close action and explicit suspend are four different things, and Caffeine
  never passes one off as another. `caffeine capabilities` shows exactly what
  this machine supports and how.
- **Honest results.** If the OS refuses an assertion, `start` fails with the
  reason and a stable exit code. It never reports a session it doesn't hold.
- **No tricks.** It never simulates keyboard or mouse input. It never blocks a
  sleep you ask for (power button, Sleep menu, `systemctl suspend`). It never
  bypasses thermal protection.
- **Reversible lid changes.** Persistent lid-close changes (Windows) need
  explicit confirmation. The original values are written to a journal first, so
  `caffeine lid restore` works even after a crash.
- **Clean up after itself.** Every assertion is tied to the process that holds
  it, so the OS releases it even after a forced kill. Stale state is detected
  and reported.
- **Built for scripts.** Every command accepts `--json` and uses
  [stable exit codes](docs/cli.md#exit-codes).
- **Extensible.** A [plugin system](docs/plugins.md) adds commands. It includes
  a built-in `build` plugin that keeps the machine awake while a build runs, and
  third-party plugins run as separate processes over a documented JSON
  protocol.
- **A configuration editor in the terminal.** `caffeine --config` works like
  nano. `caffeine config set` and `caffeine config edit` cover the same ground
  from scripts or your own editor.

## Install

From source, with Rust 1.89 or newer:

```sh
cargo install --git https://github.com/AmmaarBakshi/Caffeine caffeine-cli
```

That installs a single `caffeine` binary. It needs no root or Administrator
rights for normal use. See [docs/install.md](docs/install.md) for shell
completions, per-platform notes and uninstalling.

## Commands

| Command | Purpose |
|---|---|
| `caffeine start [--duration 2h] [--display] [--foreground]` | Start a session (detached by default) |
| `caffeine stop [--force]` | End it and release every assertion |
| `caffeine status` | Session, remaining time, lid and power state (exit 3 when idle) |
| `caffeine run -- <cmd>...` | Stay awake while a command runs, then return its exit code |
| `caffeine lid [set\|restore]` | Lid state and lid-close policy |
| `caffeine capabilities` | What this machine supports, and by which mechanism |
| `caffeine doctor` | Diagnose permissions, configuration, plugins and leftovers |
| `caffeine --config` | Interactive terminal configuration editor |
| `caffeine config path\|show\|get\|set\|edit\|validate\|...` | Scriptable configuration |
| `caffeine plugin list\|info\|enable\|disable\|install\|create\|...` | Plugins |
| `caffeine completions <shell>` | Shell completion scripts |

The full reference is in [docs/cli.md](docs/cli.md).

## Platforms

| | Windows 10/11 | macOS | Linux |
|---|---|---|---|
| Prevent idle sleep | ✓ | ✓ | ✓ (logind or desktop) |
| Keep display on | ✓ | ✓ | ✓ (desktop session) |
| Lid state | ✓ | ✓ | ✓ (logind) |
| Lid policy: read | ✓ | limited | ✓ (logind) |
| Lid policy: change persistently | ✓ (journaled) | — | — (belongs to logind.conf) |
| Lid override for one session | experimental | — | ✓ (logind inhibitor) |

[docs/compatibility.md](docs/compatibility.md) has the details, including
Modern Standby, WSL, init systems and desktop environments.

## Safety

A laptop that stays awake on battery drains faster. A laptop running with its
lid closed in a bag or on a soft surface can overheat. Caffeine prints these
warnings when they apply; turn them off with
`caffeine config set general.safety_warnings false`. A forced kill or a crash
can't always run cleanup. The OS still releases assertions, and anything that
needs restoring is reported by `caffeine doctor` and fixed by
`caffeine lid restore`. See [docs/security.md](docs/security.md).

## Documentation

- [CLI reference](docs/cli.md)
- [Configuration reference](docs/configuration.md)
- [Plugin development guide](docs/plugins.md)
- [Architecture](docs/architecture.md)
- [Platform compatibility](docs/compatibility.md)
- [Installation and uninstallation](docs/install.md)
- [Security model](docs/security.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## License

MIT. See [LICENSE](LICENSE).

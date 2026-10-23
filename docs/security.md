# Security model

Caffeine runs with your user's privileges and asks for nothing more. This page
covers what it changes, what it trusts, and what it can't protect against.

## Power management

- **Only documented, process-bound mechanisms.** Windows power requests, IOKit
  power assertions, logind and desktop-session inhibitors. The OS owns these and
  releases them when the holding process exits, even after `kill -9`, a crash
  or a lost terminal.
- **Nothing is faked.** No simulated keyboard or mouse input, no "mouse
  jiggling", no changes to screensaver timeouts, and thermal and battery
  protection are never bypassed. An explicit sleep (the power button, closing a
  MacBook's lid outside clamshell mode, the Sleep menu, `systemctl suspend`)
  always wins.
- **Success means the OS said yes.** `start` reports a session only after the
  session host has acquired every requested assertion. Any failure is reported
  with the OS error code and nothing is left half-acquired.
- **Persistent changes are explicit, confirmed and reversible.** The only
  persistent setting Caffeine can change is the Windows lid-close action, via
  `caffeine lid set` (and the opt-in experimental session override). It
  requires confirmation (or `--yes`), writes the original values to a journal
  in the durable data directory *before* changing anything, and can be undone
  with `caffeine lid restore`, including after a crash or reboot.
  `caffeine doctor` and `caffeine status` report any change that is still
  pending.
- **No elevation.** Caffeine never asks for Administrator or root rights, and
  `doctor` says when elevation would be needed (for example to run
  `powercfg /requests`). On Linux, polkit decides whether the active session
  may take logind inhibitors.

## Files

- Configuration, durable data and runtime state each live in their own
  per-user directory ([configuration.md](configuration.md#location)).
- Writes are atomic (temporary file, flush, rename) and keep a `.bak` of the
  previous configuration. A configuration file changed by another program
  since it was read is never overwritten, and an unreadable one is never
  replaced implicitly.
- Liveness of a session is decided by an OS file lock held by the session host,
  not by a PID, so PID reuse can't make Caffeine signal an unrelated process.
  `stop --force` only terminates the PID recorded by a host whose lock is still
  held.
- Secret plugin settings, and plugin settings whose names look like
  credentials, are redacted from `config show` and never logged.

## Plugins

Loading the configuration never executes plugin code. A plugin runs only when
you invoke one of its commands, or when `doctor` or `plugin info` asks an
*enabled* plugin for its health checks.

**Built-in plugins** are part of the `caffeine` binary and are as trustworthy
as Caffeine itself. They are still disabled until you enable them.

**Third-party plugins** are separate programs:

| Protection | How |
|---|---|
| No dynamic libraries | Plugins are never loaded into Caffeine's process. A crash, hang or memory bug in a plugin can't corrupt Caffeine or the session it holds. |
| Explicit trust | `plugin install` only copies files. Nothing runs until `plugin enable`, which shows the declared permissions and asks for confirmation (or `--yes`). |
| Tamper and upgrade detection | Enabling records a SHA-256 digest of the manifest and executable. Any change makes Caffeine refuse to run the plugin until it's enabled again. So does a request for permissions that weren't granted. |
| Contained executable | The executable must be a relative path inside the plugin directory, and it must still resolve inside it after following symlinks. |
| Minimal environment | Without `env.inherit`, a plugin sees only an allow-list of variables (`PATH`, `HOME`, locale, temp and system directories), not tokens such as `GITHUB_TOKEN` or `AWS_SECRET_ACCESS_KEY`. |
| No shell | Plugin executables, and the build plugin's tasks, are started with explicit argument vectors, never through a shell. |
| Bounded protocol | Protocol lines are limited to 4 MiB. Initialization and diagnostics time out, and a plugin that doesn't exit after `shutdown` is killed. |
| Name collisions | A plugin command can't shadow a built-in command or another plugin's command. Plugins that try are marked unusable. |

**What permissions can't do.** A plugin runs as you. Caffeine can't stop it
from reading your files, writing anywhere you can, or opening network
connections, whatever it declares. `process.spawn`, `fs.read`, `fs.write` and
`network` exist so you can judge whether a plugin's stated needs match its
purpose. Only `env.inherit` and `power.inhibit` are enforced by the host.
Enable only plugins whose source you trust, just as you would with any other
program.

The trust digest covers the manifest and the executable for the current
platform, not other files the executable might load at run time.

## Known limitations

- **Forced termination.** `kill -9` of a session host, a crash or a power cut
  skips cleanup. The OS still releases every process-bound assertion. The
  stale session state is detected by the host lock and removed by the next
  `caffeine stop`. A journaled lid change stays in effect until
  `caffeine lid restore` (Caffeine warns about it on every `start`, `status`
  and `doctor`).
- **Closed-lid operation is your responsibility.** Keeping a laptop running
  with the lid closed can overheat it, especially in a bag or on a soft
  surface. Caffeine warns when a session or setting allows it, and never
  promises that a particular laptop can stay awake with its lid closed. The
  firmware, the OS or a desktop environment may still sleep the machine.
- **Battery.** Keeping a machine awake on battery drains it. Configure
  `power.on_battery = "stop"` or `power.battery_threshold` to end sessions
  automatically.

## Reporting a vulnerability

Please report security issues privately to the maintainers, through GitHub's
private vulnerability reporting on the repository, rather than in a public
issue.

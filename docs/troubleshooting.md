# Troubleshooting

Start with:

```sh
caffeine doctor -v          # every check, with hints
caffeine capabilities       # what this machine supports, and why not
caffeine status             # session, lid, power, pending restores
```

Add `-vv` to any command for debug logging on stderr. Background sessions log
to the file shown by `caffeine config path --json` (`log_dir`), or to
`advanced.log_file` if set.

## The machine still went to sleep

1. **Was it an explicit sleep?** Caffeine blocks *idle* sleep only. The power
   button, the Sleep menu, `systemctl suspend`, a low-battery action or closing
   a lid all still sleep the machine. That's by design. On Windows,
   `Get-WinEvent -FilterHashtable @{LogName='System'; ProviderName='Microsoft-Windows-Kernel-Power'}`
   shows the reason for each standby ("Power Button", "Lid", "Idle Timeout").
2. **Modern Standby (Windows).** Many recent laptops enter standby when the
   display turns off, even while a system-required request is held.
   `caffeine capabilities` reports `idle-sleep` as *limited* on these
   machines. Use `caffeine start --display` (or `power.display = "keep-on"`).
3. **Is the session still running?** `caffeine status`. A session that ended
   records why ("duration elapsed", "switched to battery power", "stopped", ...), and
   `power.on_battery` or `power.battery_threshold` may have ended it.
4. **Linux desktops** sometimes run their own idle-suspend timer that ignores
   logind. Keep `advanced.backend = "auto"` (or `logind+desktop`) so both
   inhibitors are taken, and check them with `systemd-inhibit --list`.
5. **Check the OS view:** `powercfg /requests` (Windows, as Administrator),
   `pmset -g assertions` (macOS), `systemd-inhibit --list` (Linux). Each lists
   Caffeine with its `--reason` text.

## `start` fails with "not available here" (exit 4)

The capability you asked for isn't supported on this machine. The message
includes the platform's explanation. Common cases:

- **Linux without logind or a desktop session** (containers, minimal servers):
  there's nothing to inhibit. Install or enable systemd-logind or elogind, or
  run inside a desktop session.
- **`--lid keep-awake` on macOS:** macOS has no lid override. On Windows it
  needs the experimental flag:
  `caffeine config set advanced.experimental session-lid-override`.
- **`--display` on a Linux console:** display blanking there is controlled by
  the kernel (`consoleblank=`) or `setterm --blank`.

## Permission denied (exit 5)

- **Linux:** polkit refused a logind inhibitor. This usually happens over SSH
  or in a non-local session. Check `loginctl session-status` and your polkit
  rules for `org.freedesktop.login1.inhibit-*`.
- **Windows `lid set`:** group policy can lock power settings. `doctor` shows
  whether Caffeine is running elevated.
- **Directories:** `doctor` checks that the runtime and data directories are
  writable. Set `CAFFEINE_HOME` to a writable directory if they aren't.

## "A session with different settings is already active" (exit 6)

`start` with other options than the running session. Use `caffeine stop`
first, `caffeine start --replace`, or set `session.on_conflict` to `keep` or
`replace`. Repeating the *same* `start` is always fine.

## `stop` says the host did not stop in time

The session host didn't respond within `session.stop_timeout_secs`. Run
`caffeine stop --force`. If the host was killed, the OS has already released
its assertions; `caffeine stop` removes the stale state it left behind.

## Stale session after a crash, reboot or `kill -9`

`caffeine status` shows *not shut down cleanly*. The OS released the
assertions when the process died. Run `caffeine stop` to remove the state. If
`doctor` also reports a **pending restore**, a lid-close setting changed by
that session is still in effect: run `caffeine lid restore`.

## Configuration problems (exit 7)

- `caffeine config validate` lists every problem with its key.
- A broken file is never overwritten. Commands that only read settings warn and
  use defaults; commands that write refuse. Fix the file with
  `caffeine config edit` (it validates before saving), or start over with
  `caffeine config reset` (a timestamped backup is kept).
- "modified by another program since it was loaded": another editor or
  process saved the file meanwhile. Nothing was written. Run the command
  again.
- "written by a newer Caffeine": the file has a higher `schema_version`. This
  version reads it but won't modify it. Upgrade Caffeine.
- Unknown-key warnings are usually typos. `caffeine config list` shows the
  valid keys.

## Plugin problems (exit 8)

`caffeine plugin info NAME` shows the plugin's state, problems and health
checks.

| Message | Meaning |
|---|---|
| `is not enabled` / `is disabled` | `caffeine plugin enable NAME` |
| `changed since you enabled it` | The manifest or executable changed (an upgrade, a rebuild, or tampering). Review it, then enable it again. |
| `now asks for permissions you did not grant` | A new version declares more permissions. Review them and enable it again. |
| `executable ... does not exist (has the plugin been built?)` | Build it (`cargo build --release` for the template), or fix `[executable]`. |
| `requires protocol ...` | The plugin targets a different plugin protocol. Use a matching Caffeine or plugin version. |
| `collides with a built-in caffeine command` | Rename the plugin's command. |
| `did not answer within 15s` | The plugin didn't reply to `initialize`. Check its stderr output. |
| `the executable identifies as X, but the manifest declares Y` | The executable and its `caffeine-plugin.toml` don't match. Rebuild it. |

Global options go before a plugin command: `caffeine --json build`, not
`caffeine build --json`, because everything after the command name is passed
to the plugin.

## The configuration editor doesn't open

`caffeine --config` needs an interactive terminal on both stdin and stdout.
It refuses when piped or run from a script; use `caffeine config set` there. A
terminal smaller than 60×16 shows a "too small" message until it's resized.
If a crash ever leaves the terminal in a strange state, run `reset` (Unix) or
open a new window.

## Reporting a bug

Include the output of `caffeine --version`, `caffeine --json capabilities` and
`caffeine --json doctor`, plus the command you ran with `-vv`.

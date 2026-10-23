# Platform compatibility

`caffeine capabilities` reports what the machine in front of you supports. It
names the native mechanism and explains each limitation, so treat it as the
authority over this page.

Support levels: **supported**; **limited** (works with a caveat that is
printed); **requires permission**; **unsupported** (Caffeine refuses requests
that need it and says why).

## Capability matrix

| Capability | Windows 10/11 | macOS (Intel, Apple Silicon) | Linux |
|---|---|---|---|
| `idle-sleep`: prevent idle-triggered sleep | `PowerSetRequest(SystemRequired)` or `SetThreadExecutionState`. **Limited** on Modern Standby PCs (see below). | IOKit assertion `PreventUserIdleSystemSleep` | logind `Inhibit("idle")` (systemd-logind or elogind) and/or the desktop session's inhibitor. **Limited** inside WSL. |
| `display-sleep`: keep the display on | `PowerSetRequest(DisplayRequired)` | IOKit `PreventUserIdleDisplaySleep` | Desktop session inhibitors (GNOME `SessionManager`, freedesktop `ScreenSaver`). Unsupported on a bare console. |
| `lid-state` | `GUID_LIDSWITCH_STATE_CHANGE` notification | `IOPMrootDomain` `AppleClamshellState` | logind `LidClosed` (or `/proc/acpi/button/lid`) |
| `lid-policy-read` | Power scheme `SUB_BUTTONS\LIDACTION`, AC and battery | **Limited**: whether closing the lid sleeps the Mac right now (`AppleClamshellCausesSleep`); macOS has no configurable lid action | logind `HandleLidSwitch`, `…ExternalPower`, `…Docked`. A desktop environment may apply its own policy on top. |
| `lid-policy-write`: persistent change | `PowerWrite{AC,DC}ValueIndex`, journaled and reversible (`caffeine lid restore`). Group policy may deny it. | Unsupported | Unsupported by design: the policy belongs to `logind.conf` (root) or the desktop's settings, and Caffeine doesn't edit system files |
| `lid-session-override`: only during a session | **Experimental** (`advanced.experimental = ["session-lid-override"]`): temporarily changes the power scheme, journaled, restored on stop | Unsupported | logind `Inhibit("handle-lid-switch")`; released when the session ends. `keep-awake-on-ac` holds it only while on AC. |
| `power-source` | `GetSystemPowerStatus` | `IOPowerSources` | `/sys/class/power_supply` |
| `resume-detection` | Clock-gap detection in the session host | same | same |

Every assertion is bound to the process holding it, so the OS releases it if
that process dies: Windows power requests, IOKit assertions, the logind
inhibitor file descriptor, and the D-Bus connection for desktop inhibitors. The
one persistent change, Windows `lid set` (and the experimental session
override), is journaled before it is made.

## Windows

- **Windows 10 and 11** on x86_64. ARM64 builds use the same APIs but have
  not been tested. No Administrator rights are needed
  for sessions. Changing the lid action usually works without elevation unless
  group policy prevents it.
- **Modern Standby (S0 low-power idle)** PCs can enter standby when the
  display turns off, even while a system-required request is held. On these
  machines Caffeine reports `idle-sleep` as *limited* and suggests
  `caffeine start --display`.
- `powercfg /requests` (run as Administrator) lists Caffeine's requests,
  including the `--reason` text.
- Pressing the power button or choosing Sleep always sleeps the machine.
  Caffeine never blocks an explicit sleep.
- The session host is started detached, outside the terminal's console and
  job, so closing the terminal doesn't end the session.

## macOS

- Intel and Apple Silicon (aarch64), using IOKit power assertions. No special
  permissions are needed. `pmset -g assertions` lists them under the reason
  you gave.
- Closing the lid sleeps a MacBook unless it's in clamshell mode (external
  display, power and keyboard attached). macOS offers no supported per-user
  setting to change this, so Caffeine reports the lid state and whether
  closing will sleep, and never claims to change it.

## Linux

Caffeine talks to services over D-Bus, so it works the same on most
distributions. It has been designed against Arch Linux, Fedora, Ubuntu and
Debian.

| Environment | What works |
|---|---|
| systemd-logind (systemd distributions) or elogind (OpenRC, runit, …) | `idle-sleep`, `lid-state`, `lid-policy-read` and `lid-session-override` via logind inhibitors. polkit grants these to the active local session by default. |
| A desktop session (GNOME, KDE Plasma, Xfce, Cinnamon, MATE, LXQt, …) | Additionally `display-sleep`, and idle inhibition that the desktop's own idle handling respects. |
| Both | `advanced.backend = auto` takes both inhibitors for idle sleep, because some desktops implement their own idle suspend that ignores logind. |
| Neither (minimal containers, some servers) | `idle-sleep` is unsupported and `start` fails with exit 4. A machine without logind or a desktop usually has no idle suspend to prevent. |
| WSL | Inhibitors only affect the Linux side. Windows decides when the host sleeps, so `idle-sleep` is *limited* and Caffeine says to run the Windows build. |

`caffeine capabilities` shows the detected init system (`systemd`, `openrc`,
`runit`, ...), desktop environment and backend. Use `advanced.backend` to pick
`logind`, `desktop` or `logind+desktop` explicitly.

## Other platforms

On operating systems without a backend (for example FreeBSD), Caffeine builds
and runs, but every capability is reported as unsupported and sessions are
refused with exit 4. Configuration, plugins and `doctor` work as normal.

## What has been tested

- **Windows 11 Pro 25H2 (x86_64, Modern Standby laptop):** the full test suite,
  plus manual use of `start`/`stop`/`status`/`run`, the `build` plugin and
  third-party plugin install/enable/run/tamper/remove, all against real power
  requests.
- **Linux (x86_64, aarch64), macOS (x86_64, aarch64), FreeBSD:**
  cross-compiled and checked with `cargo check` and `clippy` from Windows. CI
  runs the test suite on GitHub's Ubuntu, macOS and Windows runners.

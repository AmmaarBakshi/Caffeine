# Installation and uninstallation

Caffeine is a single executable, `caffeine` (`caffeine.exe` on Windows). It
needs no root or Administrator rights for normal use, no daemon, and no
desktop environment.

## Install from source

You need Rust 1.89 or newer (`rustup update stable`).

```sh
cargo install --locked --git https://github.com/AmmaarBakshi/Caffeine caffeine-cli
```

or, from a checkout:

```sh
git clone https://github.com/AmmaarBakshi/Caffeine
cd Caffeine
cargo install --locked --path crates/caffeine-cli
```

The binary goes to `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows),
which `rustup` puts on your `PATH`. Check the installation with:

```sh
caffeine --version
caffeine capabilities
caffeine doctor
```

### Platform notes

- **Linux:** no system libraries are needed. Caffeine talks to logind and
  desktop services over D-Bus using a pure-Rust client. Idle-sleep prevention
  needs systemd-logind or elogind, or a desktop session (see
  [compatibility.md](compatibility.md#linux)).
- **macOS:** builds with the Xcode command-line tools (`xcode-select --install`).
  Links against the system IOKit and CoreFoundation frameworks.
- **Windows:** builds with the MSVC toolchain (Visual Studio Build Tools with
  the "Desktop development with C++" workload). Works in Windows Terminal,
  PowerShell and `cmd.exe`.

## Shell completion

```sh
# bash
caffeine completions bash > ~/.local/share/bash-completion/completions/caffeine
# zsh (a directory in $fpath)
caffeine completions zsh > ~/.zfunc/_caffeine
# fish
caffeine completions fish > ~/.config/fish/completions/caffeine.fish
```

```powershell
# PowerShell
caffeine completions powershell | Out-String | Add-Content $PROFILE
```

## Start sessions at login (optional)

Caffeine doesn't install any autostart entry. If you want a session at login,
add `caffeine start` to your shell profile, a systemd user unit, a launchd
agent or a Windows scheduled task. Be aware that this keeps the machine awake
whenever you're logged in.

## Upgrade

Run the install command again. The configuration file is kept and migrated
the first time a newer version saves it. Installed third-party plugins are
kept. A plugin that requires a different protocol version is reported as
unusable instead of being run.

## Uninstall

1. Stop any session, and undo any lid-policy change Caffeine made:

   ```sh
   caffeine stop
   caffeine lid restore     # only does something if `lid set` was used
   ```

2. Remove the binary:

   ```sh
   cargo uninstall caffeine-cli
   ```

3. Optionally remove its files. `caffeine config path --json` prints every
   directory. The defaults are:

   | | Linux | macOS | Windows |
   |---|---|---|---|
   | Configuration | `~/.config/caffeine` | `~/Library/Application Support/caffeine` | `%APPDATA%\caffeine` |
   | Data, plugins, trust records, restore journal | `~/.local/share/caffeine` | `~/Library/Application Support/caffeine` | `%LOCALAPPDATA%\caffeine` |
   | Logs | `~/.local/state/caffeine` | `~/Library/Logs/caffeine` | `%LOCALAPPDATA%\caffeine\logs` |
   | Runtime state | `$XDG_RUNTIME_DIR/caffeine` (cleared at logout) | in the data directory | in the data directory |

   Delete the data directory only after `caffeine lid restore` has run: the
   restore journal there is what lets Caffeine put a changed lid setting
   back. If it's already gone, set the lid-close action back in the Windows
   power options (Control Panel → Power Options → "Choose what closing the lid
   does").

Caffeine changes nothing else on the system. No services, registry entries
(apart from the power-scheme values changed by `lid set`, which
`lid restore` reverts), system files or PATH entries are created.

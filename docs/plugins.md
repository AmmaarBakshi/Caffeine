# Plugins

Plugins add commands to Caffeine without growing the core. The core works
without any plugin enabled.

- **Built-in plugins** are compiled into `caffeine`, called in-process and
  trusted implicitly. Today that's the [`build`](#the-built-in-build-plugin)
  plugin, which is disabled until you enable it.
- **Third-party plugins** are separate executables that Caffeine starts for
  each command and talks to over a [JSON protocol](#protocol) on stdin/stdout.

Both kinds implement the same Rust trait, `caffeine_plugin_api::Plugin`, so a
plugin can move from one kind to the other without changes.

## Design: why subprocesses

Two approaches were evaluated:

| | Statically linked Rust plugins | Subprocess plugins with a JSON protocol |
|---|---|---|
| Who can add one | Only by rebuilding `caffeine` | Anyone, by dropping in a directory |
| Languages | Rust | Any language that reads and writes JSON lines |
| ABI stability | Not an issue (same build) | Versioned protocol, not Rust's ABI |
| Fault isolation | A panic or hang affects Caffeine | A crash or hang is contained, timed out and killed |
| Trust model | Same as Caffeine | Reviewed and recorded per plugin |

Loading Rust dynamic libraries (`.so`/`.dylib`/`.dll`) was rejected. Rust has
no stable ABI, so a library built with another compiler version or other
dependency versions is undefined behavior. It would also run third-party code
inside the process that holds the system's power assertions. Caffeine therefore
links trusted plugins statically and runs everything else as a subprocess.

## Quick start

```sh
caffeine plugin create word-count          # writes ./caffeine-plugin-word-count
cd caffeine-plugin-word-count
cargo build --release
caffeine plugin validate .
caffeine plugin install .                  # copies files; runs nothing
caffeine plugin enable word-count          # review permissions, trust this build
caffeine word-count you
```

The generated project is the `caffeine-plugin-hello` example from this
repository with your plugin's name substituted, and Caffeine's test suite
builds and tests it. It depends on `caffeine-plugin-api` from the Caffeine git
repository. Use `--api-path ../caffeine/crates/caffeine-plugin-api` to point it
at a local checkout instead.

For development without reinstalling after every build, add the project
directory to `plugins.search_paths`:

```sh
caffeine config set plugins.search_paths "$PWD"
caffeine plugin enable word-count
```

Every rebuild changes the executable, so you have to enable the plugin again
before it runs. That's deliberate (see [Trust](#trust)).

## The manifest

Every plugin directory contains `caffeine-plugin.toml`. Unknown fields are
rejected, so a typo such as `permisions` can't make a plugin look harmless.

```toml
[plugin]
name = "word-count"            # lowercase letters, digits, single hyphens
version = "0.1.0"              # semver
description = "Counts words"
api = "^1.0"                   # protocol versions spoken (semver requirement)
caffeine = ">=0.1"             # optional: required Caffeine version
platforms = ["linux", "macos", "windows"]   # optional; default: all
permissions = ["fs.read"]
authors = ["You <you@example.com>"]          # optional
license = "MIT"                              # optional
homepage = "https://example.com"             # optional

[executable]
path = "target/release/caffeine-word-count"         # relative, forward slashes
windows = "target/release/caffeine-word-count.exe"  # optional per-OS overrides
# macos = "...", linux = "..."
args = []                                           # optional fixed arguments

[[commands]]
name = "word-count"            # also usable as `caffeine word-count`
summary = "Count words in files"
usage = "word-count FILE..."   # optional
keep_awake = false             # true: run inside an awake session (needs power.inhibit)

[[settings]]
key = "min_length"             # lowercase letters, digits, underscores
type = "integer"               # bool | string | integer | choice | string-list | path | table
description = "Ignore shorter words"
default = 1
min = 1                        # integer only
max = 100
# choices = ["a", "b"]         # choice only
# secret = true                # redact from `config show` and logs
```

`caffeine plugin validate` reports every problem at once. Errors (the plugin
can't be enabled) include: an invalid name, an incompatible protocol, an
executable path outside the plugin directory, no commands, duplicate commands
or settings, defaults that don't match their type, and `keep_awake` without
the `power.inhibit` permission. It also reports problems that depend on this
machine: an unsupported platform, a Caffeine version that doesn't match, an
executable that hasn't been built, or a command name that collides with a
built-in command or another plugin's command.

## Permissions

A third-party plugin runs with your user's privileges. Caffeine can't sandbox
it. Permissions are declarations that you review when you enable the plugin.
The two that the host mediates are enforced:

| Permission | Meaning | Enforced |
|---|---|---|
| `process.spawn` | Starts other programs | No; declaration only |
| `fs.read` | Reads files outside its directory | No; declaration only |
| `fs.write` | Writes files outside its directory | No; declaration only |
| `network` | Makes network connections | No; declaration only |
| `env.inherit` | Receives the full environment | **Yes.** Without it the plugin gets a short allow-list (`PATH`, `HOME`, locale, temp and system directories) and no tokens or credentials. |
| `power.inhibit` | Keeps the system awake while a command runs | **Yes.** `keep_awake` commands are rejected without it. |

Each plugin process also receives `CAFFEINE_PLUGIN`, `CAFFEINE_PLUGIN_DIR`,
`CAFFEINE_PLUGIN_DATA_DIR` and `CAFFEINE_PROTOCOL_VERSION`. The working
directory is the directory `caffeine` was run from.

## Trust

`caffeine plugin enable` records a SHA-256 digest of the manifest and the
executable, together with the permissions you reviewed, in
`<data>/plugins/trust.json`. Before every run the digest is computed again. If
either file has changed (an upgrade, a rebuild, tampering), or the plugin now
asks for more permissions, Caffeine refuses to run it until you enable it
again. `plugin disable` and `plugin remove` delete the record.

The digest covers the manifest and the executable for the current platform.
Other files the executable loads at run time aren't covered, so a plugin that
needs them should embed them.

## Installing and removing

`caffeine plugin install PATH` validates the plugin and then copies exactly
the manifest, the executable for this platform, and any top-level `README*`
or `LICENSE*` files into `<data>/plugins/<name>/`. Relative paths are kept, so
the manifest stays valid. The copy is staged and moved into place atomically.
Nothing is executed and nothing is enabled. `--upgrade` replaces an installed
plugin of the same name; if the files differ, you'll need to enable it again.
`caffeine plugin remove` deletes only directories inside the plugin directory.

## Protocol

Version 1.0. The host starts the executable with the manifest's `args`, a
clean environment (see above), stdin and stdout piped, and stderr connected to
the user's terminal. Messages are JSON objects, one per line, each with a
`type` field. A line may be at most 4 MiB.

```text
host                                     plugin
 | -- initialize -----------------------> |
 | <---------------------------- ready -- |   or failed
 | -- invoke ---------------------------> |
 | <---- log / output / data (any number) |
 | <--------------------------- result -- |   or failed
 | -- shutdown -------------------------> |   plugin exits
```

`diagnose` can be sent instead of `invoke`; it's answered with `diagnostics`.

### Host to plugin

```json
{"type":"initialize","protocol":"1.0.0",
 "host":{"name":"caffeine","version":"0.1.0","os":"linux","arch":"x86_64"},
 "settings":{"min_length":3},"permissions":["fs.read"],
 "data_dir":"/home/me/.local/share/caffeine/plugin-data/word-count"}
{"type":"invoke","command":"word-count","args":["a.txt"],"cwd":"/home/me","json":false}
{"type":"diagnose"}
{"type":"shutdown"}
```

`settings` contains the manifest defaults overlaid with the user's
`[plugins.settings.<name>]`, already validated. When `json` is true the user
asked for `--json`: report results with `data` messages.

### Plugin to host

```json
{"type":"ready","protocol":"1.0.0","name":"word-count","version":"0.1.0"}
{"type":"log","level":"info","message":"counting 1 file"}
{"type":"output","text":"a.txt: 1234 words"}
{"type":"data","value":{"a.txt":1234}}
{"type":"result","exit_code":0}
{"type":"failed","error":{"message":"a.txt: not found","hint":"check the path"}}
{"type":"diagnostics","checks":[{"id":"wc.dict","status":"ok","summary":"dictionary loaded"}]}
```

- `ready` must repeat the manifest's `name` and `version`; otherwise the host
  refuses the plugin, because the executable doesn't match its manifest.
- `log` levels are `error`, `warn`, `info`, `debug` and `trace`. Log messages
  go to Caffeine's log and to stderr with `-v`.
- `output` is a line of human-readable output; it's suppressed by `--json` and
  `--quiet`. With `--json` the host prints one document whose `data` field is
  the single `data` value, or an array if there were several.
- `result.exit_code` becomes the exit code of `caffeine`.
- Diagnostic `status` is `ok`, `info`, `warn` or `fail`; `hint` is optional.

### Rules

- Never write anything but protocol messages to stdout. Programs the plugin
  starts must not inherit stdout. Point their stdout at stderr instead
  (`caffeine_plugin_api::sdk::child_stdout()` does this in Rust).
- The plugin must answer `initialize` within 15 s and `diagnose` within 30 s.
  `invoke` has no time limit. After `shutdown`, or when stdin closes, the
  plugin must exit within 5 s or it's killed.
- Within protocol major version 1, hosts and plugins ignore unknown fields and
  unknown message types. A new major version may change anything; the host
  checks `api` in the manifest and the `protocol` in `ready`.
- Non-JSON lines on stdout are passed through as output with a debug log entry.
  Don't depend on this.

## Writing a plugin in Rust

```rust
use caffeine_plugin_api::{Host, Invocation, Manifest, Plugin, PluginError};

const MANIFEST: &str = include_str!("../caffeine-plugin.toml");

struct WordCount;

impl Plugin for WordCount {
    fn manifest(&self) -> Manifest {
        Manifest::from_toml(MANIFEST).expect("bundled manifest is valid")
    }

    fn invoke(&mut self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
        for file in &inv.args {
            let text = std::fs::read_to_string(inv.cwd.join(file))
                .map_err(|e| PluginError::new(format!("{file}: {e}")))?;
            host.print(&format!("{file}: {} words", text.split_whitespace().count()));
        }
        Ok(0)
    }
}

fn main() -> std::process::ExitCode {
    caffeine_plugin_api::sdk::serve(WordCount)
}
```

`initialize`, `diagnose` and `shutdown` have default implementations. To test
a plugin without Caffeine, feed `sdk::serve_io` request lines and inspect the
replies. The SDK's own tests show how.

Plugins in other languages implement the protocol above directly. A plugin is
just an executable that reads lines from stdin and writes lines to stdout.

## The built-in `build` plugin

`caffeine plugin enable build` adds two commands:

- `caffeine build [TASK] [ARGS...]` runs a task while keeping the system awake.
- `caffeine build-list` shows configured tasks and detected build systems.

### Tasks

```toml
[plugins.settings.build]
default_task = "release"        # used by a bare `caffeine build`
detect = true                   # offer detected build systems

[plugins.settings.build.tasks.release]
program = "cargo"               # looked up in PATH (PATHEXT on Windows), or a path
args = ["build", "--release"]
cwd = "."                       # relative to where caffeine runs; optional
env = { RUSTFLAGS = "-D warnings" }   # added to the environment; optional
description = "Optimized build" # optional
```

A task's program is executed directly with its arguments, never through a
shell. Extra arguments after the task name are appended to its arguments, so
`caffeine build release --locked` runs `cargo build --release --locked`. With
`--json`, the build's own stdout is redirected to stderr so the JSON document
stays parseable.

`caffeine build` with no task name runs `default_task` if it's set, otherwise
the only configured task, otherwise the only detected build system. If there
are several to choose from, it lists them and asks you to pick one.

### Detection

Detection looks only at the current directory itself, never at parents or
subdirectories. It recognizes exactly these systems, and each one runs one
fixed command. It never configures or generates a project.

| Name | Detected when the directory contains | Runs |
|---|---|---|
| `cargo` | `Cargo.toml` | `cargo build` |
| `npm` | `package.json` with a `build` script | `npm run build` |
| `pnpm` | as `npm`, plus `pnpm-lock.yaml` | `pnpm run build` |
| `yarn` | as `npm`, plus `yarn.lock` | `yarn run build` |
| `make` | `GNUmakefile`, `makefile` or `Makefile` | `make` |
| `cmake` | `CMakeLists.txt` and an already configured `build/CMakeCache.txt` | `cmake --build build` |
| `go` | `go.mod` | `go build ./...` |
| `gradle` | `gradlew` (`gradlew.bat` on Windows) | `./gradlew build` |
| `maven` | `pom.xml` | `mvn package` |

Configured tasks take precedence over detected systems of the same name.
`caffeine build cargo --release` runs the detected Cargo build with
`--release` appended. Set `detect = false` to use configured tasks only.
`caffeine doctor` checks that every configured task's program can be found.

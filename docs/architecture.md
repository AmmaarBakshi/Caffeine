# Architecture

## Workspace

```
caffeine-cli ──────────┬──► caffeine-core            (types, config, sessions; no OS calls)
  (bin: caffeine)      ├──► caffeine-platform        (native backends) ──► caffeine-core
                       ├──► caffeine-tui             (--config editor) ──► core, plugin-manager
                       ├──► caffeine-plugin-manager  (discovery, trust, running) ──► core, plugin-api
                       └──► caffeine-plugin-build    (built-in plugin) ──► plugin-api

caffeine-plugin-api     (the stable contract third-party plugins compile against; depends only on serde/semver)
caffeine-plugin-hello   (example third-party plugin; the `plugin create` template)
```

| Crate | Responsibility |
|---|---|
| `caffeine-core` | Error model and exit codes, durations, paths, the capability model and `PowerBackend` trait, the versioned configuration (schema, settings registry, format-preserving editing, migration, atomic saving), and the session store and host loop. Contains no platform code, so all of it is unit-testable with a fake backend (`testing` feature). |
| `caffeine-platform` | One backend per OS behind `caffeine_platform::native()`, plus process helpers (detaching, liveness, forced kill) and the restore journal. |
| `caffeine-cli` | Argument parsing (clap), output (human or JSON, color), logging, and the commands. Thin: decisions live in core, mechanisms in platform. |
| `caffeine-tui` | The `caffeine --config` editor (ratatui + crossterm), split into a row model, a terminal-independent state machine, and a renderer. |
| `caffeine-plugin-api` | Manifest, permissions, the `Plugin` trait, the JSON Lines protocol and the SDK that serves a `Plugin` over it. |
| `caffeine-plugin-manager` | Discovery, validation, trust records, environment construction, install/remove, scaffolding and the protocol client. |
| `caffeine-plugin-build` | The built-in `build` plugin. |

## Sessions

A session is owned by exactly one **session host** process.

```
caffeine start                         caffeine __host (detached)
──────────────                         ─────────────────────────
plan: flags + config → AwakeRequest
preflight: capability report
take start.lock
probe existing session ──► conflict policy
spawn host, send HostSpec (stdin) ───► take session.lock (exclusive OS lock)
                                       backend.acquire(request) → guard (RAII)
                                       write session.json
wait for readiness line  ◄──────────── {"ok":true,"session":{...}}  (stdout)
report "Awake ..."; exit                loop every 250 ms:
                                         stop-request? termination signal?
caffeine stop                            deadline? battery policy? (every 30 s)
─────────────                            clock gap > 15 s ⇒ resumed: refresh guard
write stop-request(id) ───────────────► heartbeat session.json (every 30 s)
wait for session.lock to be released   drop guard (assertions released)
                                       remove session.json, write last-session.json
```

- **Readiness, not hope.** `start` returns success only after the host reports
  that the assertions are held. If the host fails, its structured error is
  sent back and shown with its exit code.
- **Liveness by lock.** `session.lock` is held with an exclusive OS file lock
  for the host's whole lifetime. The kernel drops it when the process dies, so
  "can I take the lock?" tells whether a host is alive, without trusting PIDs.
  A `session.json` without a locked owner is *stale*. It's reported, and
  removed by the next `stop` or `start`.
- **Process-bound assertions.** Every backend guard wraps an OS resource that
  the OS releases on process exit: a power request handle, an IOKit assertion,
  a logind inhibitor file descriptor, or a D-Bus connection. Dropping the guard
  releases it early. `CompositeGuard` releases its parts in reverse order and
  collects errors.
- **Foreground modes.** `start --foreground`, `run` and `keep_awake` plugin
  commands run the same host loop in the calling process, with an extra end
  condition (Ctrl+C, the child exiting, the plugin thread finishing).
- **Detaching.** The host gets a new session on Unix (`setsid`). On Windows it
  gets `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, with job breakaway where
  allowed, and the caller's stdio handles are made non-inheritable while it's
  spawned, so closing or capturing the terminal doesn't affect it.
- **Runtime vs durable state.** `session.json` and the locks live in the
  runtime directory. `last-session.json`, the restore journal and plugin trust
  live in the durable data directory. The configuration file is never touched
  by sessions.

## Backends

```rust
pub trait PowerBackend: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> CapabilityReport;   // what, how, and why not
    fn acquire(&self, request: &AwakeRequest) -> Result<Box<dyn AwakeGuard>>;
    fn lid_status(&self) -> Result<LidStatus>;
    fn lid_policy(&self) -> Result<LidPolicy>;
    fn set_lid_policy(&self, change: &LidPolicyChange) -> Result<LidPolicy>;
    fn power_source(&self) -> Result<PowerSource>;
    fn diagnose(&self) -> Vec<DiagnosticCheck>;
    fn pending_recovery(&self) -> Result<Vec<RecoveryItem>>;
    fn recover(&self) -> Result<Vec<String>>;
}
```

`AwakeRequest` keeps idle sleep, display sleep and lid handling as separate
fields, and `Capability` has a separate variant for each. A backend never
satisfies one with another. `acquire` either gets everything requested or
releases what it got and fails.

Persistent changes (only the Windows lid action) go through the **restore
journal**. The original value is written to durable storage before the change,
removed after a successful restore, and replayed by `lid restore`. An entry
belonging to a session whose host is gone is marked *left by crash*.

## Configuration

```
config.toml ──parse──► toml_edit::DocumentMut ──(migrate)──► document
                                │                                 │
                                └──── deserialize + validate ─────┴──► Config (typed)
edit(key, value): parse by SettingDef → apply to a copy of the document →
                  re-deserialize + validate → commit, or leave unchanged
save: refuse if read-only or changed on disk → atomic write + .bak
```

One **settings registry** (`config::settings::all()`) holds each setting's
key, section, label, help, type and required capabilities. It drives
`config get/set/list`, the generated default file, unknown-key detection and
the interactive editor, so all four always agree.

## Plugins

```
discovery (reads manifests only)
  built-ins ─┐
  <data>/plugins/* ─┼─► PluginEntry { manifest, origin, executable, enabled, trust, problems }
  search_paths ─┘        problems = manifest.validate() + platform/version compatibility
                                    + executable containment + command collisions
                                    + settings schema validation

run(name, command):
  enabled? no errors? trust == digest(manifest, executable)? permissions granted?
  ├─ built-in: Plugin::initialize → invoke → shutdown (in-process)
  └─ third-party: spawn(exe, args, cwd, env allow-list) ─ JSON Lines ─► SDK ─► same Plugin trait
  keep_awake? → wrapped in a foreground session (session::keep_awake_while)
```

The reasons for choosing subprocesses over dynamic libraries are in
[plugins.md](plugins.md#design-why-subprocesses).

## Errors and output

`caffeine_core::Error` carries an `ErrorKind`, a message, an optional hint and
an optional OS error code. The kind maps to a stable exit code. In `--json`
mode every command prints exactly one JSON document, with errors as
`{"error": {...}}` and warnings collected into `warnings`. Foreground sessions
print JSON Lines events. Logs go to stderr (interactive commands) or to a
rotating file (background hosts), never to stdout.

## Testing

- Core logic (durations, config editing and migration, the session loop with a
  fake backend and a fake clock, the store's locking) is unit-tested without
  touching the OS.
- Platform backends have unit tests for parsing (sysfs, D-Bus replies, power
  schemes) and are type-checked for every target in CI.
- `caffeine-plugin-manager/tests` drives the real example plugin through
  discovery, trust, tampering, install/upgrade/remove and settings, and builds
  a freshly generated starter project.
- `caffeine-cli/tests` runs the real binary in an isolated `CAFFEINE_HOME`,
  including a real session where the platform allows it.
- The editor's state machine and rendering are tested with synthetic key
  events and ratatui's `TestBackend`.

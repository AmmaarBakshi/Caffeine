You are a senior Rust systems programmer and open-source CLI architect. Build **Caffeine**, a serious, cross-platform, terminal-only power-management utility.

## 1. Project philosophy

Caffeine is a command-line tool for Windows 10/11, macOS, and Linux distributions.

It must have:
- No GUI.
- No graphical settings window.
- No desktop dashboard.
- No web-based configuration interface.
- No requirement for a graphical desktop environment.

The primary implementation language is Rust. Favor reliability, native operating-system integration, minimal resource usage, maintainability, and excellent CLI ergonomics.

The project should feel like a well-engineered Unix utility, not an Electron application disguised as a terminal tool.

## 2. Core functionality

Implement native power-management backends for:

- Windows 10 and Windows 11.
- macOS on supported Intel and Apple Silicon systems.
- Linux distributions, including Arch Linux, Fedora, Ubuntu, and Debian, with capability detection for different init systems and desktop environments.

Provide commands to:
- Prevent idle-triggered sleep.
- Stop an awake session.
- Run a session for a specified duration.
- Inspect the current session and remaining time.
- Detect lid state when supported.
- Inspect lid-close policy where available.
- Configure lid-close behavior through explicit, reversible operations where supported.
- Report power-management capabilities.
- Diagnose permissions and platform-specific errors.

Separate idle-sleep prevention, display sleep, lid-close behavior, and actual system suspend. Never represent one capability as another.

Never report success when the underlying OS operation failed. Do not bypass hardware thermal safeguards or simulate user input to keep the computer awake.

## 3. CLI design

Implement an intuitive command structure, adapting syntax only where technically necessary:

- `caffeine start`
- `caffeine stop`
- `caffeine status`
- `caffeine start --duration 2h`
- `caffeine lid`
- `caffeine capabilities`
- `caffeine doctor`
- `caffeine --config`
- `caffeine plugin list`
- `caffeine plugin enable <name>`
- `caffeine plugin disable <name>`
- `caffeine plugin info <name>`

Support useful command-line help, version information, shell completion where practical, meaningful exit codes, and machine-readable output such as JSON for automation.

Use consistent errors and concise terminal output. Avoid unnecessary banners and decorative output in scripting mode.

## 4. Nano-inspired configuration editor

The command `caffeine --config` must open an interactive, terminal-based configuration editor.

This is not a graphical interface. It should feel familiar to users of Nano and other terminal utilities.

Use an appropriate Rust terminal library, evaluating options such as Ratatui and Crossterm.

The configuration editor should display settings in a readable terminal layout and support keyboard navigation, editing, toggling, saving, cancelling, and contextual help.

Organize configuration into sections:

### General
- Startup behavior.
- Default awake-session duration.
- Logging verbosity.
- Notification preferences, limited to supported terminal mechanisms.

### Power management
- Idle-sleep prevention.
- Display-sleep policy.
- Lid-state detection.
- Lid-close policy management.
- AC-power and battery preferences where supported.

### Sessions
- Default session duration.
- Maximum optional duration.
- Behavior when a session expires.
- Session status and lifecycle preferences.

### Plugins
- Installed plugins.
- Enabled or disabled state.
- Plugin-specific settings.
- Plugin compatibility and permission requirements.

### Advanced
- Platform backend selection where appropriate.
- Diagnostic options.
- Configuration paths.
- Logging paths.
- Experimental features.

The editor must show which settings are unsupported on the current platform. Do not display a working toggle for an operation that cannot be performed.

Use safe configuration parsing, validation, and atomic file writes. Preserve user settings when upgrading versions. Never silently overwrite a configuration file.

Allow a conventional editor-based workflow where useful, but provide a built-in terminal editor so that Nano is not a required dependency.

Support a command such as `caffeine config path` to print the configuration file location.

## 5. Extensible plugin architecture

Build a real plugin system rather than hard-coding every future feature into the main application.

Define a stable Rust-facing plugin interface for:
- Plugin identification and versioning.
- Capability declarations.
- Plugin configuration schemas.
- Command registration.
- Initialization and shutdown.
- Logging and diagnostic reporting.
- Compatibility checks.
- Permission declarations.

The core must remain usable without any optional plugins.

Support listing, inspecting, enabling, disabling, installing, and removing plugins through the CLI, with appropriate safeguards.

Design the plugin architecture so that additional modules can provide new commands or workflows, including development tools and build helpers.

For example, a future build plugin could register commands to invoke configured build tools or run a project's existing build tasks. Such operations must be explicit and must not execute arbitrary plugin code merely because a configuration file was loaded.

Prefer a versioned plugin protocol with a well-defined boundary. Evaluate statically linked Rust plugins and subprocess-based plugins using a documented JSON protocol before selecting an implementation. Avoid assuming that arbitrary Rust dynamic libraries are safe or portable across operating systems.

Validate plugin manifests, API versions, permissions, and configuration. Do not automatically execute newly installed plugins. Clearly distinguish trusted built-in plugins from third-party extensions.

## 6. Plugin development workflow

Provide a way for developers to inspect plugin templates and create new plugins.

Consider commands such as:

- `caffeine plugin create <name>`
- `caffeine plugin validate <path>`
- `caffeine plugin info <name>`
- `caffeine plugin run <name> -- <arguments>`

Choose the exact interface based on the plugin architecture.

The development workflow should generate a documented starter project, explain how commands are registered, validate manifests, and provide a repeatable build and test process.

Plugins that invoke build systems must use explicitly configured executables and arguments. Avoid shell-string execution where direct process arguments are sufficient. Make working directories, environment variables, and execution permissions explicit.

Do not invent a build plugin that claims to compile arbitrary projects automatically without defining supported build systems and project detection behavior.

## 7. Rust architecture

Use a Cargo workspace with clear module boundaries, such as:

- `caffeine-core`: session management, shared types, and configuration.
- `caffeine-cli`: argument parsing and command dispatch.
- `caffeine-config`: interactive terminal editor.
- `caffeine-platform`: native platform abstractions and capability reporting.
- `caffeine-plugin-api`: versioned plugin contracts.
- `caffeine-plugin-manager`: plugin discovery and lifecycle.
- Platform-specific backend modules.
- Integration tests and developer tooling.

These names are suggestions; optimize the actual workspace structure rather than creating unnecessary crates.

Use RAII for native resources, reliable cleanup for awake sessions, structured errors, and tests for lifecycle edge cases. Avoid unnecessary dependencies and excessive abstraction.

## 8. Configuration and persistence

Use a documented, human-readable configuration format such as TOML.

Support:
- A platform-appropriate configuration directory.
- Validated defaults.
- Schema or version migration.
- Explicit overrides.
- Safe writes and recovery from malformed files.
- Plugin-specific configuration.
- A command to print effective configuration without exposing secrets.

Keep runtime state separate from persistent configuration. Do not store transient awake-session state as though it were a permanent power policy.

## 9. Reliability and safety

Implement:
- Idempotent session start and stop.
- Correct handling of multiple session requests.
- Duration expiration.
- Cleanup when the application exits normally.
- Graceful handling of termination and sleep/resume events where supported.
- Clear errors for permission failures.
- Safe restoration of modified settings when restoration is possible.
- Warnings about battery drain, ventilation, and closed-lid heat.

Do not silently modify global power settings, require administrator privileges unnecessarily, or promise that every laptop can remain awake with its lid closed.

A forced kill or system crash may prevent cleanup. Document these cases and provide a recovery or diagnostic mechanism where feasible.

## 10. Documentation and quality

Produce:
- README.
- Architecture documentation.
- CLI reference.
- Configuration reference.
- Plugin development guide.
- Platform compatibility matrix.
- Installation and uninstallation instructions.
- Security model.
- Troubleshooting guide.
- Tests and continuous integration workflows.

Implement real functionality rather than placeholder commands or mock success responses.

Inspect the existing repository before modifying files. Preserve useful code and existing user changes. Run builds and tests on available platforms, and clearly report which platforms were actually tested.

## Execution strategy

First inspect the repository and propose a concise implementation plan. Then implement the Rust CLI and core power-management engine, followed by the configuration editor, plugin system, and cross-platform validation.

Work incrementally. Build and test after each major stage. Resolve actual failures instead of merely documenting them.

Do not add a graphical interface at any stage. Do not publish a release or push commits without explicit authorization.
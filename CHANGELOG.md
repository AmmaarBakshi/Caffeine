# Changelog

All notable changes to Caffeine are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/). The `--json` output, the exit
codes, the configuration file and the plugin protocol are the public
interface that versions describe.

## [Unreleased]

The first version, 0.1.0. It hasn't been published yet.

### Added

- `caffeine start`, `stop`, `status` and `run` keep the system awake with
  native mechanisms: power requests on Windows, IOKit assertions on macOS, and
  logind or desktop-session inhibitors on Linux. Sessions have optional
  durations, survive the terminal closing, notice resume from sleep, can stop
  on battery, and are idempotent.
- Recovery after a crash or forced kill: `status` and `doctor` report stale
  sessions, and `caffeine stop` cleans them up.
- `caffeine lid` reports the lid state and lid-close policy. On Windows,
  `caffeine lid set` changes the policy after journaling the original values,
  and `caffeine lid restore` reverts them.
- `caffeine capabilities` and `caffeine doctor` report what the platform
  supports and diagnose permission and configuration problems.
- TOML configuration with validation, schema migration, comment-preserving
  edits and atomic writes, via `caffeine config ...` or the terminal editor
  opened by `caffeine --config`.
- Plugins: a versioned JSON Lines protocol for third-party executables,
  manifest validation, permission review, digest-based trust, and
  `caffeine plugin list/info/enable/disable/install/remove/create/validate/run`.
- The built-in `build` plugin runs configured build tasks or a detected build
  system while keeping the system awake.
- `--json` output on every command, documented exit codes, and shell
  completions.
- Documentation: README, architecture, CLI and configuration references,
  plugin guide, compatibility matrix, installation, security model and
  troubleshooting.

[Unreleased]: https://github.com/AmmaarBakshi/Caffeine/commits/main

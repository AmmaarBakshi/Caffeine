# Contributing

## Build and test

```sh
cargo build                       # debug build of the `caffeine` binary
cargo test --workspace            # all tests (some take real OS assertions)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Run the binary against a throwaway home so your real configuration and
sessions are untouched:

```sh
CAFFEINE_HOME=$(mktemp -d) cargo run -- doctor
```

Check the platforms you aren't on (install the targets once with
`rustup target add`):

```sh
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu
cargo clippy --workspace --all-targets --target aarch64-apple-darwin
cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc
```

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs formatting,
clippy, tests, a release build and a smoke test on Linux, macOS and Windows.
It also checks the MSRV (1.89), FreeBSD and aarch64 Linux, and that rustdoc
builds without warnings.

## Where things live

See [docs/architecture.md](docs/architecture.md). In short: decisions and
data structures go in `caffeine-core` (no OS calls, testable with
`caffeine_core::testing::FakeBackend`), OS mechanisms go in
`caffeine-platform`, and `caffeine-cli` stays thin.

## Rules of the road

- Never report success for an OS operation that failed, and never present one
  capability (idle sleep, display, lid, suspend) as another.
- Anything persistent that the OS doesn't clean up by itself must be journaled
  before it's changed, and must be restorable.
- New settings go in `config::settings::all()` with their help text and
  required capabilities. The CLI, the generated default file and the editor
  pick them up from there.
- Plugin protocol changes that aren't backwards compatible need a new major
  `PROTOCOL_VERSION`.
- Keep the `--json` output of existing commands backwards compatible: add
  fields, don't rename or remove them.

## Commits

Conventional commit style (`feat(scope): ...`, `fix(scope): ...`,
`docs: ...`). Keep each commit focused and building.

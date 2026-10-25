# caffeine-plugin-hello

The example third-party plugin. `caffeine plugin create NAME` generates a new
project from these files with `hello` replaced by `NAME`, and the test suite
builds and runs this plugin, so it always matches the current protocol.

It provides two commands:

- `caffeine hello [NAME]...` prints a greeting using the `greeting` setting.
- `caffeine hello-wait SECONDS` keeps the system awake while it waits, which
  shows how `keep_awake = true` and the `power.inhibit` permission work.

## Try it from this repository

Inside the workspace, cargo builds into the workspace's `target/` directory,
but the manifest points at `target/release/` next to `caffeine-plugin.toml`
(where a standalone project's build lands). Build it into its own target
directory so the paths line up:

```sh
cargo build --release -p caffeine-plugin-hello --target-dir crates/caffeine-plugin-hello/target
cargo run -- plugin validate crates/caffeine-plugin-hello
cargo run -- plugin install crates/caffeine-plugin-hello    # copies files; runs nothing
cargo run -- plugin enable hello                            # review and trust this build
cargo run -- hello Ada
cargo run -- plugin remove hello
```

Use `CAFFEINE_HOME=$(mktemp -d)` to keep this away from your real plugins and
configuration.

See [docs/plugins.md](../../docs/plugins.md) for the manifest, permissions,
trust model and protocol.

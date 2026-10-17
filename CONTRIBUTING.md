# Contributing

Thanks for your interest. Bug reports, fixes and small focused improvements are welcome. For larger changes, open an issue first so we can agree on the approach.

## Toolchain

- Rust 1.88 or newer (stable), with `rustfmt`
- Node.js 22 or newer
- Python 3 (for the layer check)
- Linux; the runtime uses systemd and Unix sockets

Docker works too: `docker build -t lambda-memory:runtime .` runs the Rust tests and builds a release binary.

## Checks

Run these before opening a pull request; CI runs the same:

```bash
cargo fmt --check
cargo test --all-features --locked
node --test tests/*.test.mjs
python3 scripts/check_layers.py
```

Plain `cargo test` skips the feature-gated runtime and MCP suites, so use `--all-features`. Tests make no network calls and need no API key.

## Architecture

The Rust code follows the [MASA framework](https://www.masa-framework.org/): `domain_models`, `engines`, `integrations`, `services` and `delivery`, with one-way dependencies. Engines are pure (no I/O, no clock). Read [MASA.md](MASA.md) for how the layers map to Rust; `scripts/check_layers.py` enforces the boundaries. [docs/architecture.md](docs/architecture.md) describes the pipeline.

## Commits and pull requests

- Commit messages: `area: short summary` in the imperative, for example `web: fix timeline paging` or `runtime: report socket path errors`. Keep each commit focused.
- One topic per pull request. Describe what changed and why, and how you tested it.
- Add or update tests for behaviour changes.
- Update the README, `docs/` or `skills/` when user-visible behaviour, tools or settings change.
- Don't add dependencies without discussing it first.

## Test data

Use synthetic data only, such as `project-demo` and invented decisions. Never commit real memories, stores, tokens, API keys or configuration files from your own installation.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).

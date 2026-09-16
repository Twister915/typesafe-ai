# Contributing

This repository contains a single Rust library crate. Start with
[the Rust style guide](docs/rust-style.md) and [AGENTS.md](AGENTS.md).
The checked-in toolchain file selects nightly Rust with rustfmt and Clippy.

## Local checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test --no-default-features
cargo test --no-default-features --features reqwest
cargo test --no-default-features --features ureq
cargo test --no-default-features --features ureq,rustls-tls
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo package
```

Run `cargo fmt --all` to apply formatting. Tests use local HTTP fixtures and do not
need an API key. Doctests compile their API examples without sending live requests.
Keep `Cargo.lock` checked in for repeatable development; downstream applications
resolve dependencies using the version ranges in `Cargo.toml`.

## Changes

Keep changes focused. Explain the observable behavior and include the checks you
ran. Add table-based tests for wire formats, validation, error handling, and retry
behavior when those change. Use the live TypeSafe documentation to confirm API
contracts, including structured question descriptions and optional usage counts.

Public APIs need concise rustdoc and practical examples. Prefer additive changes;
call out changes to request serialization, default settings, or error behavior.
Never commit API keys, private request data, or live-service test fixtures.

## Dependencies and releases

Add dependencies only when they remove meaningful maintenance work. Follow the
style guide's Cargo declaration format and use the smallest required feature set.
The default TLS feature uses reqwest's maintained rustls integration; its crypto
backend may include native code. This crate requires `std`; only the reqwest backend
requires Tokio. Keep DTOs and client traits available without either backend. Verify
that enabling only `ureq` does not activate reqwest or Tokio in the normal dependency
tree:

```sh
cargo tree --no-default-features --features ureq,rustls-tls -e normal
```

Add release notes under `Unreleased` in `CHANGELOG.md`; leave version bumps to the
manual GitHub release workflow. `main` stays open for development, while release
tags preserve each released version. See [the release guide](docs/releases.md) for
the GitHub button, validation, failure recovery, and local publishing commands.
Before publishing locally, review the contents with `cargo package --list`.
Publishing requires the maintainer's explicit authorization and a crates.io
account with access to the package name.

Unless explicitly stated otherwise, contributions are licensed under the
[Apache License, Version 2.0](LICENSE).

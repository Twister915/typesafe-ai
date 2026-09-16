# Working on typesafe-ai

Read and follow [Joey's Rust Style Guide](docs/rust-style.md) before changing Rust code.
This repository is one library crate, not a Cargo workspace. Use nightly Rust and
edition 2024. Keep the public API small and document every public item with concise
rustdoc; examples should compile as doctests where possible.

## Design boundaries

- Keep shared DTOs, sync/async client traits, and errors independent of HTTP
  backends. Put reqwest and ureq implementations behind separate optional features
  with public exports at the crate root. This remains a single crate.
- A build without default features must support DTOs and traits alone. A ureq-only
  build must not depend on reqwest or Tokio in its normal dependency graph.
- Configure both backends with the shared `ClientConfig` struct and simple
  constructors. Do not add separate backend builders or generated DTO builders
  without a concrete need. Keep the API key separate from printable configuration.
- Check the [live TypeSafe API reference](https://docs.typesafe.ai/api) and
  [structured question reference](https://docs.typesafe.ai/primitives/advanced)
  before changing wire formats. Preserve structured criteria and optional usage.
- Prefer concrete types, static dispatch, and existing dependency capabilities.
  Check current stable releases before adding dependencies. Use block-form Cargo
  dependency declarations whenever configuring more than a version.
- Keep credentials out of logs, errors, examples, and debug output. Never use real
  credentials in tests. Use loopback HTTP fixtures for transport behavior.
- Document defaults and retry behavior precisely. Do not introduce hidden retries
  or retry ambiguous failures without considering duplicate requests.
- Keep evaluation events lazy. Iterators and streams own retry deadlines; consuming
  events drives work and dropping them stops future attempts. Implement `evaluate`
  by driving events, so there is only one implementation of retry behavior per backend.
- Keep transport errors concrete through `Error<E>` and backend aliases. Do not
  replace them with boxed dynamic errors or mutually exclusive backend features.
- This library requires `std`; do not add a cosmetic `no_std` feature. Only the
  async reqwest backend requires Tokio. Keep TLS features independent of transport
  selection and avoid activating an unused backend through its TLS feature.

## Verification

Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test --all-features`, and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`.
Test the feature combinations in `.github/workflows/ci.yml` when changing transport
features. Inspect `cargo tree --no-default-features --features ureq,rustls-tls -e normal`
to verify the blocking backend's dependency isolation.
Use `cargo package` to verify release contents and buildability before release.
Tests must remain independent of the live TypeSafe service.

## Collaboration

When delegation is requested, keep the team small. Use `gpt-6-astra` with high
reasoning for planning and design, and `gpt-5.6-sol` with high reasoning for
implementation and documentation. Give each agent distinct file ownership.

Do not publish a release or invent repository URLs, ownership claims, or credentials.

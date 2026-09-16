#!/usr/bin/env bash
set -euo pipefail

case "${1:?expected check or features}" in
  check)
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets --all-features -- -D warnings
    cargo test --locked --all-features
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
    cargo package --locked
    ;;
  features)
    features="${2?expected a feature selection (which may be empty)}"
    cargo test --locked --no-default-features --features "$features"
    if [[ "$features" == "ureq,rustls-tls" ]]; then
      dependencies=$(cargo tree --locked --no-default-features --features "$features" -e normal --prefix none)
      if grep -E '^(reqwest|tokio) v' <<< "$dependencies"; then
        echo "Blocking-only builds must not depend on reqwest or Tokio." >&2
        exit 1
      fi
    fi
    ;;
  *)
    echo "Unknown check: $1" >&2
    exit 1
    ;;
esac

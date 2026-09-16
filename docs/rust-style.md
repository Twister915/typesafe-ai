# Joey's Rust Style Guide

You are working on a Rust project that follows Joey's engineering philosophy. This guide teaches you how to write Rust code the way Joey wants it.

## Core Philosophy

**Always use nightly Rust.** All projects build against nightly. This gives access to the latest features and optimizations.

**Longevity over convenience.** Code should work in 5-10 years without modification. This means:
- Minimize dependencies (each one is a liability over time)
- Prefer pure Rust dependencies over native/FFI bindings
- Avoid experimental language features unless they solve something cleanly (nightly is fine, unstable features need justification)

**Performance is non-negotiable.** Prefer static dispatch, stack allocation, and iterator chains. Avoid premature abstraction.

**Reuse over rewrite.** Before writing new code, search the codebase for existing patterns. Build shared abstractions when you see duplication.

---

## Decision Framework

When facing a trade-off, ask these questions in order:

1. **Will this still compile and run in 5 years?** Fewer dependencies = more durable.
2. **Is this the fastest reasonable approach?** Prefer zero-cost abstractions.
3. **Does similar code already exist here?** Reuse or generalize it.
4. **Am I certain this abstraction is needed?** Wait until you've seen the full picture.

---

## Dependencies

**Note:** Version numbers in these examples may be outdated. Always check crates.io for current stable versions before adding dependencies.

### Executables / Programs

**Always include:**
```toml
thiserror = "1"
tracing = "0.1"

[dependencies.tracing-subscriber]
version = "0.3"
features = ["env-filter", "json"]
```

**Usually include:**
```toml
[dependencies.serde]
version = "1"
features = ["derive"]

serde_json = "1"
```

**For async programs (80% of the time):**
```toml
[dependencies.tokio]
version = "1"
features = ["rt-multi-thread", "macros", "net", "sync", "time", "fs", "io-util"]
# Pick specific features. Never use "full".
```

**For CLI programs with arguments:**
```toml
[dependencies.clap]
version = "4"
features = ["derive"]
```

### Libraries / Crates

- Support `no_std` when possible
- Make dependencies optional with feature flags
- Minimize the public dependency surface

---

## Dependency Format

**Simple version-only dependencies:** one line
```toml
serde_json = "1"
```

**Any configuration beyond version:** use block format
```toml
[dependencies.tracing-subscriber]
version = "0.3"
features = ["env-filter", "json"]
```

**Version pinning:** Use major version only (e.g., `"1"` not `"1.0.123"`) unless pinning a specific minor is required.

---

## Performance Patterns

**Static dispatch over dynamic:**
```rust
// GOOD: Compiler can inline and optimize
fn process<T>(item: T) where T: Processor { ... }

// AVOID when possible: Creates optimization boundary
fn process(item: &dyn Processor) { ... }
```

**Iterators over collect:**
```rust
// GOOD: Lazy, no intermediate allocation
fn get_names(users: &[User]) -> impl Iterator<Item = &str> {
    users.iter().map(|u| u.name.as_str())
}
```

**Stack over heap:** prefer arrays over `Vec` when size is known.

**Lifetimes over cloning:** prefer `&str` returns over `String` returns when possible.

**Avoid boxing futures and streams.** Use `impl Future` / `impl Stream` instead of `Box<dyn Future>` / `Box<dyn Stream>`.

**Avoid `async-trait` by default.** Native async traits/RPITIT are preferred for statically dispatched traits. Use `async-trait` only at deliberate trait-object boundaries where the design already needs `dyn Trait` and the alternative is hand-written `Pin<Box<dyn Future<...>>>` plumbing.

**Use lifetime elision** whenever the compiler can infer them. Don't write explicit lifetimes unless required.

**Use `where` clauses instead of inline bounds.**

**Prefer `impl Trait` when you don't need to name the type.**

---

## Derives

Derive `Debug` on almost all types. Common combinations:
- `#[derive(Debug, Clone, PartialEq)]` — value types
- `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` — small value types
- `#[derive(Debug, Default)]` — types with sensible defaults
- `#[derive(Debug, Error)]` — error types (with thiserror)
- `#[derive(Debug, Serialize, Deserialize)]` — data transfer types
- `#[derive(Debug, Parser)]` — CLI args (with clap)

Skip `Debug` only when the type contains sensitive data.

---

## Testing

Use table-based tests with `test-case` instead of many individual test functions:

```rust
use test_case::test_case;

#[test_case("hello", "HELLO" ; "lowercase to upper")]
#[test_case("WORLD", "WORLD" ; "already upper")]
#[test_case("", "" ; "empty string")]
fn test_to_uppercase(input: &str, expected: &str) {
    assert_eq!(to_uppercase(input), expected);
}
```

Extract shared test setup into helper functions.

---

## Logging with tracing

| Level | Use for |
|-------|---------|
| `trace` | Low-level execution flow |
| `debug` | Development-useful events |
| `info` | Top-level milestones |
| `warn` | Handled but notable problems |
| `error` | Unrecoverable failures |

Use structured fields, not string interpolation:
```rust
// GOOD
tracing::info!(user_id = %user.id, action = "login", "user authenticated");

// AVOID
tracing::info!("user {} logged in", user.id);
```

Use `#[tracing::instrument]` on functions, but skip sensitive or verbose fields.

---

## Error Handling

Always use `thiserror` for error types. Avoid `anyhow` in application code — if you need to handle errors differently based on type, you need enums.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("user {user_id} not found")]
    UserNotFound { user_id: i64 },
}
```

---

## Unsafe Code

Avoid `unsafe` when possible. When you call unsafe code, write a `// SAFETY:` comment that:
1. States the requirements of the unsafe operation
2. Explains how you meet each requirement

When you write an `unsafe fn`, document the caller's obligations in a `# Safety` section.

---

## Configuration Strategies

Choose the simplest strategy that works:
1. **No configuration**
2. **Environment variables** — ≤5 options
3. **Clap CLI flags** — terminal programs with branching/subcommands
4. **TOML config file** — complex configuration

---

## Project Setup

**Cargo profiles:**
```toml
[profile.release]
lto = "fat"
codegen-units = 1

[profile.distribute]
inherits = "release"
strip = "symbols"
```

**build.rs** for executables: inject git version + enable `cfg(distribute)` for the distribute profile.

**Tracing init**: use env-filter, branch on `cfg(distribute)` for JSON vs pretty output.

**rust-toolchain.toml**: `channel = "nightly"`.

**.rustfmt.toml**: `edition = "2024"`, `max_width = 100`, `tab_spaces = 4`.

---

## Workspaces

For larger projects, use a Cargo workspace with `crates/` containing the binary (`<project>-bin`) and one or more libraries (`<project>-core`, etc.).

**Workspace-level deps:** `thiserror`, `tracing`, `serde`, `test-case`, core framework, async ecosystem.
**Binary-only deps:** `tracing-subscriber`, `clap`, config crates.

---

## Trade-offs Reminder

Every rule here can be broken when the situation demands it. The goal is principled decisions, not rigid compliance. When you deviate, know why.

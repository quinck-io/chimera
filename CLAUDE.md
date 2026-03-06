# CLAUDE.md

## Project Overview

**chimera** is a CLI daemon that manages multiple GitHub Actions self-hosted runners on a single Debian machine.

You run it with a config file. It spins up N runner instances, each of which is a fully protocol-compatible drop-in replacement for the official GitHub Actions runner. Each instance connects to GitHub's broker, polls for jobs, executes them, and streams logs back — all concurrently.

The key behavior: if a workflow job has a `container:` tag, the job runs inside that Docker container. If not, it runs directly on the host machine, exactly like the official runner. Services (`services:`) always run as Docker containers.

The end result is a single binary — `chimera` — that replaces the official runner on any machine where you'd normally run multiple `./run.sh` instances.

The `PLAN.md` file is a detailed roadmap for how to build this. It breaks down the project into phases, each with specific tasks and milestones. The code snippets in `PLAN.md` are illustrative, not prescriptive — you can implement the same behavior in a cleaner or simpler way if you prefer.

---

## Rust Conventions

It is important you structure the project in a clean, layered way. It is important to separate concerns and avoid tight coupling. It is important to write code that is easy to read and understand. 

Before writing things and couple or scatter one logic in multiple places, think about how to structure it in a clean way. Do not exhagerate abstractions, but do not just throw everything in one file either.

### Error Handling

Use `anyhow::Result` at function boundaries. Use `thiserror` to define typed errors inside modules when callers need to match on error variants. Never use `.unwrap()` or `.expect()` in library or daemon code — only acceptable in tests or truly unreachable branches (with a comment explaining why).

```rust
// bad
let val = map.get("key").unwrap();

// good
let val = map.get("key").context("missing required key")?;

// also fine in tests
assert_eq!(result.unwrap(), expected);
```

Propagate errors with `?`. Add context with `.context("what we were doing")` when it helps debugging. Don't swallow errors silently — log and continue, or propagate, but never drop.

Do not add useless comments. Only comment why, not what. If the code isn't self-explanatory, comment it. If something weird is necessary, explain why. But don't comment things that are obvious from the code itself.

### Lifetimes

Use as few lifetimes as possible. Prefer owned types (`String`, `Vec<T>`, `Arc<T>`) over borrowed types in structs. Only introduce lifetime parameters when the borrow is genuinely required for performance and the code stays readable. If you're fighting the borrow checker to make a lifetime work, clone instead.

### Async

Use `tokio` throughout. Prefer `async fn` over manually constructed futures. Don't block the async runtime — use `tokio::task::spawn_blocking` for CPU-heavy or blocking I/O work. Keep `async` functions focused; extract sync helper functions where the logic doesn't need to be async.

### Code Style

Write code that reads like prose. Prioritize clarity over cleverness. If a piece of code needs a comment to be understood, it's fine to add one — but prefer making the code self-explanatory first.

- Flat is better than nested. Early returns over deep `if/else` chains.
- Small, focused functions. If a function is doing two things, split it.
- Descriptive variable names. `conn` is fine for a connection, `x` is not fine for anything.
- Avoid over-engineering. Don't add abstraction layers that don't exist yet.
- No dead code. If you write it, wire it up.

### Module Structure

Use the modern Rust module convention: name files after the module, not `mod.rs`.

```
// good
src/broker/session.rs
src/broker/poller.rs

// bad
src/broker/mod.rs  (only acceptable as a thin re-export file, not for logic)
```

Keep module boundaries meaningful. A module should represent a coherent concept, not just a file for organizing functions.

### Dependencies

The crate list in `PLAN.md` is just a starting point. If you find a better library for a task, feel free to use it. Just make sure to keep dependencies minimal and well-justified. Each new crate adds maintenance overhead, so only add what you need. Also, always ask user before. Install new versions if not already installed, dont use PLAN.md versions blindly.

---

## Architecture Guidance

The `PLAN.md` file is a reference, not a contract. The directory layout, struct names, and code snippets in it are illustrative. You are free — and expected — to choose a cleaner or simpler approach if one exists. What matters is that the behavior matches the spec, not that the code looks like the plan.

When in doubt, simpler wins. Don't build abstractions speculatively. Build what's needed for the current phase.

---

## Testing

Write unit tests for almost everything that has logic. If a function has a branch, edge case, or non-trivial transformation, it should have a test.

Don't mock internal modules. Test them directly. Mock only external HTTP APIs using `wiremock`.

Integration or e2e tests only when explicitly requested. Default to unit tests.

Tests live in a separate file next to the module they test, named `{module}_test.rs`. For example, `src/config.rs` has tests in `src/config_test.rs`, and `src/broker/auth.rs` has tests in `src/broker/auth_test.rs`. Each test file is included at the bottom of the main module file using the `#[path]` attribute:

```rust
#[cfg(test)]
#[path = "auth_test.rs"]
mod auth_test;
```

The `#[path]` attribute is required because Rust's module system would otherwise look for the file in a subdirectory.

Tests that require Docker are tagged `#[ignore]` so they don't run in environments without a socket, but do run in CI.

Keep tests readable. A test should be obvious about what it's checking. Arrange / Act / Assert with a blank line between sections is a good default structure.

---

## Before Marking Any Task Done

1. Run `cargo build` — must compile with zero errors.
2. Run `cargo clippy -- -D warnings` — must pass with zero warnings.
3. Run `cargo test` — all tests must pass.

If any of these fail, fix the issues before declaring the task complete. Do not leave `#[allow(dead_code)]` or `#[allow(unused)]` as a workaround — fix the underlying issue.
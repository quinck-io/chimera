# Contributing to Chimera

Thanks for your interest in contributing! This document covers what you need to know.

## Getting started

1. Fork the repo and clone your fork
2. Install [Rust](https://rustup.rs/) (stable toolchain)
3. Install Docker (required for container-related tests, skipped by default in local)
4. Run `cargo build` to verify everything compiles

## Development workflow

```bash
cargo build                        # compile
cargo clippy -- -D warnings        # lint (must pass with zero warnings)
cargo test                         # run tests (skips Docker tests by default)
cargo test -- --ignored            # run Docker tests (requires Docker)
```

All three checks must pass before submitting a PR.

## Submitting changes

1. Create a branch from `main`
2. Make your changes in focused, well-scoped commits
3. Write tests for any new logic or behavior changes
4. Open a pull request against `main`

### PR guidelines

- Keep PRs focused on a single change
- Include a clear description of what and why
- Link any related issues
- Make sure CI is green before requesting review

## Testing

Chimera has two layers of tests:

### Unit tests (`src/**/*_test.rs`)

Module-level tests next to the code they test. These cover individual functions, parsing, expression evaluation, etc. See `CLAUDE.md` for conventions.

### Integration tests (`tests/`)

End-to-end tests that exercise the execution engine by constructing job manifests, running them through `run_all_steps()`, and asserting on the job conclusion. Steps are real bash scripts that self-assert (exit non-zero on failure).

```
tests/
  common/mod.rs              # shared harness (TestEnv, manifest/step builders)
  basics_test.rs             # env vars, file propagation, outputs
  conditions_test.rs         # if:, continue-on-error, always(), job.status
  expressions_test.rs        # expression language (bracket, wildcard, functions, operators)
  hashfiles_test.rs          # hashFiles() function
  needs_test.rs              # needs context resolution
  secrets_test.rs            # secret injection
  workflow_commands_test.rs   # ::set-env::, ::set-output::, ::add-path::, etc.
  composite_test.rs          # local composite action execution
  matrix_test.rs             # matrix context resolution
  docker_test.rs             # container mode, services, docker actions (#[ignore])
  timeout_test.rs            # step timeout kill (#[ignore])
```

Docker and timeout tests are marked `#[ignore]` and run separately:

```bash
cargo test                    # unit + integration (no Docker)
cargo test -- --ignored       # Docker + timeout tests only
```

**When adding a new feature**, write integration tests if it affects job execution behavior (new expression function, new workflow command, new step type, etc.). Use the harness in `tests/common/mod.rs` — it provides `TestEnv::setup()`, manifest builders, and step builders.

### Manual smoke tests (`chimera-test.yml`)

The workflow `.github/workflows/chimera-test.yml` is a manual-trigger (`workflow_dispatch`) smoke test that runs real GitHub Actions jobs on a chimera runner. It covers things that integration tests cannot:

- **Node.js actions** (`actions/checkout`, `actions/cache`, `setup-rust-toolchain`) — these require network access, Node.js runtime, and real GitHub API calls.
- **Cache round-trips** — `actions/cache` talks to chimera's cache server; testing the full save/restore cycle requires the cache server running.
- **Full build in container** — building chimera itself inside a Docker container with `cargo build` / `cargo test`.
- **Repo Docker actions** — custom Docker actions from the repository that need checkout first.
- **Artifact upload/download** — requires the GitHub artifact API and Node.js actions.
- **Reusable workflows** — a GitHub-level feature (workflow_call trigger) outside chimera's execution scope.

These tests require a machine running chimera registered as a self-hosted runner. They are not part of CI and are triggered manually when needed. Refer to a maintainer if you want to run or modify these tests, as they require access to the self-hosted runner and secrets.

## Code style

- Follow the conventions in `CLAUDE.md` (error handling, async patterns, module structure)
- Use `anyhow::Result` at function boundaries, `thiserror` for typed errors
- No `.unwrap()` or `.expect()` outside of tests
- Prefer clarity over cleverness
- Tests go in `{module}_test.rs` files next to the module they test

## Security vulnerabilities

Please report security issues privately — see [SECURITY.md](SECURITY.md).

## Reporting bugs

Open an issue with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Chimera version (`chimera --version`) and OS

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

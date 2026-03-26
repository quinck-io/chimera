# Contributing to Chimera

Thanks for your interest in contributing! This document covers what you need to know.

## Getting started

1. Fork the repo and clone your fork
2. Install [Rust](https://rustup.rs/) (stable toolchain)
3. Install Docker (required for container-related tests)
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

## Code style

- Follow the conventions in `CLAUDE.md` (error handling, async patterns, module structure)
- Use `anyhow::Result` at function boundaries, `thiserror` for typed errors
- No `.unwrap()` or `.expect()` outside of tests
- Prefer clarity over cleverness
- Tests go in `{module}_test.rs` files next to the module they test

## Reporting bugs

Open an issue with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Chimera version (`chimera --version`) and OS

## Security vulnerabilities

Please report security issues privately — see [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

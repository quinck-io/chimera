# chimera

Protocol-compatible GitHub Actions runner replacement, written from scratch in Rust.

Chimera is a single, fast binary that manages multiple runners concurrently. Run it as a systemd service, in a Docker container, or just in a terminal. It speaks the same registration and job execution protocol as the official runner, so it **works with any existing workflow without modification**.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/quinck-io/chimera/main/install.sh | sh
```

Or install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/quinck-io/chimera/main/install.sh | sh -s -- v0.1.0
```

Prebuilt binaries are available for Linux and macOS (x86_64 and aarch64) on the [releases page](https://github.com/quinck-io/chimera/releases).

### Build from source

```bash
git clone https://github.com/quinck-io/chimera.git
cd chimera
cargo build --release
# binary is at target/release/chimera
```

## Why?

Official GitHub Actions runners are slow, resource-heavy, leak memory and difficult to manage. 

Chimera is designed to be a better experience for self-hosted runners, with a focus on performance, reliability and multirunner management, which are especially important for larger organizations. It also serves as a reference implementation of the GitHub Actions runner protocol, which is currently only documented through reverse engineering.

See [docs/gh-protocol.md](docs/gh-protocol.md) for the full spec, API and auth flows of the GitHub Actions runner protocol.

## Usage

Register runners the same way you would with the official runner, then start the daemon. Runners poll for jobs concurrently — each job gets a clean workspace and can use Docker containers as needed.

```
chimera register --url https://github.com/org/repo --token AABBC... --name runner-0
chimera register --url https://github.com/org/repo --token DDEEF... --name runner-1
chimera start
```

Jobs with `container:` run inside Docker. Jobs without it run on the host. Services always run as containers on a shared bridge network. Logs stream live to the GitHub UI.

All state and data is stored in `~/.chimera` by default.

## CLI

```
chimera register --url <url> --token <token> --name <name> [--labels a,b] [--root ~/.chimera]
chimera unregister --name <name> [--root ~/.chimera]
chimera start [--root ~/.chimera]
chimera status [--root ~/.chimera]
```

**register** — Register a runner with GitHub. Token comes from Settings > Actions > Runners.

**unregister** — Remove a runner from GitHub and delete its local credentials.

**start** — Start all registered runners concurrently. 

**status** — Show daemon uptime, per-runner phase (Idle/Running/Stopped), and current job info.

## Config

`~/.chimera/config.toml` (managed by `register`):

```toml
runners = ["runner-0", "runner-1"]

[daemon]
log_format = "text"           # "text" or "json" (json works well with journald)
shutdown_timeout_secs = 300
```

## Supported features

- Host and container step execution (`run:`, `container:`, `services:`)
- All action types: Node.js, Docker, composite
- Full `${{ }}` expressions — `success()`, `failure()`, `hashFiles()`, `contains()`, `format()`, all contexts
- All workflow commands (`set-output`, `set-env`, `add-mask`, `save-state`, etc.)
- Step conditions, timeouts, `continue-on-error`, cancellation
- Per-job Docker network, port mapping, volumes, `--privileged`/`--cap-add`
- Live log streaming, job outputs, heartbeats
- Support for `actions/cache/v4`

Chimera-only features:
- Multi-runner concurrency with independent error isolation
- Local `actions/cache` server for faster caching and no external dependencies
- Automatic cleanup of old workspaces, containers and orphaned processes
- Configurable LRU cache (default 10GB)

## Out of scope or unsupported features

- GHES (GitHub Enterprise Server)
- Windows — not in scope but may work (untested)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)

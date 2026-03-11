# chimera

Protocol-compatible GitHub Actions runner replacement, written from scratch in Rust.

Chimera is a single, fast binary that manages multiple runners concurrently. Run it as a systemd service, in a Docker container, or just in a terminal. It speaks the same registration and job execution protocol as the official runner, so it **works with any existing workflow without modification**.

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
- Multi-runner concurrency with independent error isolation

## Unsupported features

- GHES (GitHub Enterprise Server)
- Windows — not in scope but may work (untested)

## License

MIT

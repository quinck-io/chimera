# chimera — Master Implementation Plan

A from-scratch GitHub Actions self-hosted runner daemon written in Rust.  
Codename: **chimera**  
Target platform: **Debian only**. Multi-runner. Docker-aware. Fast cache.

---

## Core Behavior

Chimera runs N concurrent runner instances on one machine. Each is a protocol-compatible drop-in for the official GitHub Actions runner.

**Execution model mirrors the official runner exactly:**
- No `container:` tag in workflow → job runs directly on the host machine (processes, paths, tools — everything just like the official runner)
- `container:` tag present → job runs inside that Docker container, workspace bind-mounted in
- `services:` always run as Docker containers regardless of job execution mode

This is not an abstraction on top of Docker. Host execution IS host execution. The machine chimera runs on is the runner environment.

---

## Installation Layout

Everything lives under `~/.chimera/`. Delete the folder and chimera is completely gone — no system files touched.

```
~/.chimera/
├── config.toml          # main config
├── bin/
│   └── chimera          # the binary (symlinked from install location)
├── work/
│   └── {runner-name}/
│       └── {repo}/
│           └── {repo}/  # GITHUB_WORKSPACE for that runner's current job
├── cache/
│   ├── entries/         # CacheEntry JSON files
│   ├── data/            # content-addressed blobs (blake3 + zstd)
│   └── tmp/             # in-progress uploads
├── tmp/                 # RUNNER_TEMP per runner
├── logs/
│   └── {runner-name}/   # local runner logs (rotated)
└── tool-cache/          # RUNNER_TOOL_CACHE (shared across runners)
```

Install command:
```
chimera install              # creates ~/.chimera/, writes config.toml template, registers runners
chimera uninstall            # removes ~/.chimera/, deregisters runners from GitHub
chimera status               # show all runner states
```

---

## Reference Sources

- **Protocol (most important):** https://depot.dev/blog/github-actions-runner-architecture-part-1-the-listener
- **Official runner source (C#):** https://github.com/actions/runner
  - `src/Runner.Listener/MessageListener.cs`
  - `src/Runner.Listener/JobDispatcher.cs`
  - `src/Runner.Worker/Worker.cs`
  - `src/Runner.Sdk/VssConnection.cs`
- **Go reimplementation (edge cases):** https://github.com/ChristopherHX/github-act-runner
- **JIT API docs:** https://docs.github.com/en/rest/actions/self-hosted-runners?apiVersion=2022-11-28#create-configuration-for-a-just-in-time-runner-for-an-organization
- **Cache API (exact compat target):** https://github.com/actions/toolkit/tree/main/packages/cache/src — specifically `cacheHttpClient.ts`
- **bollard examples:** https://github.com/fussybeaver/bollard/tree/master/examples
- **Azure DevOps log API:** https://learn.microsoft.com/en-us/rest/api/azure/devops/distributedtask/logs/create

---

## Non-Goals

- Windows / macOS support
- The official runner's forked Worker subprocess model
- Auto-update of the runner binary
- Full Actions expression evaluator (use pre-evaluated fields from job manifest)
- GHES support
- Kubernetes / ARC
- Running every job in a container by default (not what we're building)

---

## Crate Choices (locked in)

```toml
tokio              = { version = "1", features = ["full"] }
reqwest            = { version = "0.12", features = ["json", "rustls-tls", "stream"], default-features = false }
bollard            = "0.17"
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
serde_yaml         = "0.9"
rsa                = { version = "0.9", features = ["pem", "sha2"] }
jsonwebtoken       = "9"
base64             = "0.22"
blake3             = "1"
zstd               = "0.13"
clap               = { version = "4", features = ["derive", "env"] }
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
anyhow             = "1"
thiserror          = "1"
uuid               = { version = "1", features = ["v4"] }
chrono             = { version = "0.4", features = ["serde"] }
scopeguard         = "1"
tokio-util         = { version = "0.7", features = ["io"] }
bytes              = "1"
futures            = "0.3"
axum               = "0.7"
tower              = "0.4"
wiremock           = "0.6"    # dev
tempfile           = "3"      # dev
```

---

## Top-Level Architecture

```
~/.chimera/config.toml
        │
        ▼
┌─────────────────────────────────────────────────────────────────┐
│                        chimera daemon                           │
│                                                                 │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   │
│  │ Runner 0 │   │ Runner 1 │   │ Runner 2 │   │ Runner N │   │
│  │          │   │          │   │          │   │          │   │
│  │ broker   │   │ broker   │   │ broker   │   │ broker   │   │
│  │ session  │   │ session  │   │ session  │   │ session  │   │
│  │          │   │          │   │          │   │          │   │
│  │ executor │   │ executor │   │ executor │   │ executor │   │
│  │host|dock │   │host|dock │   │host|dock │   │host|dock │   │
│  └────┬─────┘   └────┬─────┘   └────┬─────┘   └────┬─────┘   │
│       └──────────────┴──────────────┴──────────────┘           │
│                     shared services                             │
│  ┌─────────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │  Cache Manager  │  │ Docker Pool  │  │   Web UI / API   │  │
│  │  rolling xGB    │  │ image cache  │  │   :8080          │  │
│  └─────────────────┘  └──────────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Binary Layout

```
chimera/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── daemon.rs               # spawn N runners, own shared services, handle SIGTERM
│   ├── config.rs               # ~/.chimera/config.toml load/save, validation, paths
│   ├── install.rs              # install/uninstall subcommands, dir scaffolding
│   │
│   ├── runner/
│   │   ├── mod.rs              # RunnerInstance: lifecycle, state machine
│   │   ├── state.rs            # RunnerState: Idle/Running/Draining/Error
│   │   └── registry.rs         # RunnerRegistry: Arc<RwLock<Vec<RunnerHandle>>>
│   │
│   ├── broker/
│   │   ├── mod.rs
│   │   ├── auth.rs             # JWT sign + OAuth token exchange
│   │   ├── session.rs          # POST /sessions, DELETE /sessions
│   │   └── poller.rs           # GET /message long-poll loop
│   │
│   ├── job/
│   │   ├── mod.rs              # Job orchestration, top-level run_job()
│   │   ├── acquire.rs          # POST /acquirejob
│   │   ├── renew.rs            # Heartbeat task (every 60s)
│   │   ├── complete.rs         # POST /completejob
│   │   └── schema.rs           # Full job manifest types
│   │
│   ├── executor/
│   │   ├── mod.rs              # Executor trait, dispatch host vs docker
│   │   ├── host.rs             # Direct host execution (no container:)
│   │   ├── docker.rs           # Container execution (container: present)
│   │   ├── services.rs         # Service container lifecycle (always Docker)
│   │   ├── resources.rs        # JobResources RAII: networks/containers/volumes
│   │   └── commands.rs         # Workflow command parser (::set-env:: etc.)
│   │
│   ├── logs/
│   │   ├── mod.rs
│   │   ├── pager.rs            # Batched upload to Azure Pipelines log API
│   │   └── timeline.rs         # Step timeline PATCH
│   │
│   ├── cache/
│   │   ├── mod.rs
│   │   ├── manager.rs          # CacheManager: rolling LRU, eviction, stats
│   │   ├── store.rs            # Content-addressed blob store (blake3 + zstd)
│   │   ├── server.rs           # axum: actions/cache@v3 compat HTTP API
│   │   └── docker_cache.rs     # Image layer cache, pull dedup, LRU eviction
│   │
│   ├── web/
│   │   ├── mod.rs
│   │   ├── api.rs              # REST API handlers
│   │   └── ui.rs               # Inline single-file HTML dashboard (no build step)
│   │
│   └── util/
│       ├── workspace.rs        # Per-job work dir: create, wipe, paths
│       └── cgroups.rs          # cgroup v2 limits (Phase 7)
│
└── tests/
    ├── broker_test.rs
    ├── cache_test.rs
    ├── docker_test.rs
    ├── executor_host_test.rs
    └── web_api_test.rs
```

---

## Configuration File (`~/.chimera/config.toml`)

```toml
[daemon]
log_format  = "text"      # "text" | "json"
web_port    = 8080
web_enabled = true

[cache]
max_gb     = 50           # rolling LRU eviction above this
cache_port = 9999         # local cache HTTP server port

[docker]
socket                   = "/var/run/docker.sock"
prune_images_older_than_days = 7

# One [[runner]] block per concurrent runner instance.
# jit_config is a base64 JIT token from the GitHub API.
# Can also be set via env: CHIMERA_RUNNER_0_JIT_CONFIG etc.

[[runner]]
name       = "chimera-0"
jit_config = "base64..."
labels     = ["self-hosted", "Linux", "X64"]

[[runner]]
name       = "chimera-1"
jit_config = "base64..."
labels     = ["self-hosted", "Linux", "X64"]
```

---

## Execution Model (Critical — Read This)

### No `container:` tag → Host Execution

```yaml
jobs:
  build:
    runs-on: ["self-hosted", "Linux", "X64"]
    steps:
      - run: cargo build --release
```

Steps run as child processes directly on the host machine. This is identical to how the official runner works. The user's PATH, installed tools, system libraries — all available exactly as-is.

Chimera does:
- Creates `~/.chimera/work/{runner}/{repo}/{repo}/` as workspace
- Sets all `GITHUB_*` env vars on child processes
- Captures stdout/stderr and streams to GitHub log API
- Parses `::workflow-commands::` from stdout
- Cleans up workspace after job

Chimera does NOT:
- Sandbox the process in any special way (same as official runner)
- Wrap it in Docker
- Restrict filesystem access

### `container:` tag → Docker Execution

```yaml
jobs:
  build:
    runs-on: ["self-hosted", "Linux", "X64"]
    container:
      image: ubuntu:latest
      env:
        FOO: bar
    steps:
      - run: cargo build --release
```

Steps run inside the specified Docker container. Chimera:
- Pulls (or uses cached) the container image
- Creates per-job bridge network
- Starts the job container with workspace bind-mounted to `/github/workspace`
- Runs each step as `docker exec` into that container
- Sets `GITHUB_*` env vars inside the container
- Streams logs back exactly the same way as host execution
- Tears down container + network after job

### `services:` → Always Docker (regardless of job mode)

```yaml
services:
  postgres:
    image: postgres:15
    env:
      POSTGRES_PASSWORD: secret
    ports:
      - 5432:5432
```

Service containers are started before the job steps, attached to the job's bridge network. In host execution mode, they're accessible via `localhost` (port mapped) or service name (via hosts file entry). In container mode, accessible via service name directly on the shared network.

---

## Job Execution Dispatch Logic

```rust
pub enum ExecutionMode {
    Host,                    // no container: in workflow
    Container(ContainerSpec) // container: present, has image + optional env/volumes
}

impl ExecutionMode {
    pub fn from_job_manifest(manifest: &JobManifest) -> Self {
        match &manifest.job_container {
            Some(spec) => Self::Container(spec.clone()),
            None => Self::Host,
        }
    }
}

pub async fn run_job(manifest: JobManifest, ctx: JobContext) -> Result<Conclusion> {
    let mode = ExecutionMode::from_job_manifest(&manifest);

    // Services always start regardless of mode
    let mut resources = JobResources::new(&ctx.docker, &manifest.job_id).await?;
    resources.start_services(&manifest.service_containers).await?;

    let conclusion = match mode {
        ExecutionMode::Host => {
            host::run_steps(&manifest.steps, &resources, &ctx).await
        }
        ExecutionMode::Container(spec) => {
            docker::run_steps_in_container(&spec, &manifest.steps, &resources, &ctx).await
        }
    };

    resources.cleanup().await;  // always
    conclusion
}
```

---

## Docker Image Cache

### Pull Deduplication

Before any `docker pull`:
1. `docker inspect <image>` — if present locally, compare digest
2. Query registry API for current digest of the tag (cache this response for 1 hour)
3. If digests match → skip pull entirely
4. If mismatch or not present → pull, update cached digest

```rust
pub struct DockerCache {
    docker: Docker,
    digest_cache: Arc<RwLock<HashMap<String, DigestEntry>>>,
    digest_ttl: Duration,  // default 1 hour
}

pub struct DigestEntry {
    digest: String,
    checked_at: Instant,
}

impl DockerCache {
    pub async fn ensure_image(&self, image: &str) -> Result<()>
    pub async fn record_use(&self, image: &str)
    pub async fn evict_lru(&self, target_bytes: u64) -> Result<u64>
    pub async fn total_size_bytes(&self) -> Result<u64>
}
```

### Local Registry Mirror (optional but recommended)

Chimera can optionally configure a pull-through registry mirror. When enabled:
- Starts `registry:2` container on first run (or checks if already running)
- Writes `/etc/docker/daemon.json` with `registry-mirrors` (requires sudo, opt-in)
- First pull of any image layer hits internet; subsequent pulls (any runner, any job) hit local disk
- Completely transparent to jobs — they don't know the mirror exists

This is documented but NOT auto-configured. Users opt in via:
```toml
[docker]
enable_registry_mirror = true   # requires sudo for daemon.json
mirror_port = 5000
```

---

## Shared Cache Manager

One `CacheManager` shared across all runner instances. Enforces `max_gb` via rolling LRU eviction.

```rust
pub struct CacheManager {
    store: Arc<BlobStore>,
    entries: Arc<RwLock<EntryMap>>,
    max_bytes: u64,
    current_bytes: Arc<AtomicU64>,
    stats: Arc<CacheStats>,
}

pub struct CacheStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub evictions: AtomicU64,
    pub total_bytes: AtomicU64,
}
```

**Blob deduplication:** blake3 hash of raw content = storage key. Two cache entries with identical content (e.g. same node_modules on two branches) share one blob on disk. Reference counted — blob deleted only when last referencing entry is evicted.

**Eviction:** On every commit, if over limit, evict LRU entries until under limit. Never evict an entry accessed in the last 60s (in-flight restore).

**Persistence:** Entries survive daemon restart. On startup, scan `~/.chimera/cache/entries/` and rebuild in-memory state.

---

## Cache Server (actions/cache@v3 compat)

One axum HTTP server on `localhost:{cache_port}`, shared by all runners.

All job environments get `ACTIONS_CACHE_URL=http://localhost:{cache_port}/`.

`actions/cache@v3` reads this env var and talks to our server directly — zero workflow changes needed.

```
GET  /_apis/artifactcache/cache?keys={k1,k2}&version={v}
     → 200 { cacheKey, archiveLocation } | 204 (miss)

POST /_apis/artifactcache/caches
     body: { key, version, cacheSize? }
     → 200 { cacheId }

PATCH/_apis/artifactcache/caches/{id}
     headers: Content-Range: bytes {start}-{end}/*
     body: raw bytes
     → 204

POST /_apis/artifactcache/caches/{id}    (commit)
     body: { size }
     → 204

GET  /download/{blake3_hash}             (our download URL)
     → 200 raw bytes (decompressed)
```

Lookup semantics (must match GitHub exactly):
1. Exact `key` + `version` match → return it
2. Longest prefix of `key` with same `version` → return it
3. Miss → 204

---

## Full Protocol Reference

### Auth Flow

```
1. POST /repos/{org}/{repo}/actions/runners/generate-jitconfig
   → { encoded_jit_config: "<base64>" }

2. base64-decode → JSON:
   {
     AgentId, AgentName,
     ServerUrl: "https://pipelinesghubeus*.actions.githubusercontent.com/TOKEN/",
     ServerUrlV2: "https://broker.actions.githubusercontent.com/",
     UseV2Flow: "true",
     GitHubUrl, WorkFolder,
     // + RSA private key PEM + AuthorizationUrl
   }

3. RSA private key → sign JWT → POST AuthorizationUrl → Bearer token
   (token used for all broker + pipelines API calls)
```

### Broker Session

```
POST https://broker.actions.githubusercontent.com/sessions
Authorization: Bearer {token}

{
  "sessionId": "<new uuid>",
  "ownerName": "{hostname} (PID: {pid})",
  "agent": {
    "id": {AgentId},
    "name": "{AgentName}",
    "version": "2.327.1",    ← must be recent or broker returns 400
    "osDescription": "Debian GNU/Linux",
    "ephemeral": true,
    "status": 0
  },
  "useFipsEncryption": false
}
→ { "sessionId": "..." }
```

### Long-Poll Loop

```
GET https://broker.actions.githubusercontent.com/message
  ?sessionId={sid}&status=Online&runnerVersion=2.327.1
  &os=Linux&architecture=X64&disableUpdate=true
Authorization: Bearer {token}
(client timeout: 55s, server holds up to 50s)

→ 202 + empty: no job, loop immediately
→ 200 + { messageId, messageType: "RunnerJobRequest", body: "{...}" }

body: { runner_request_id, run_service_url, billing_owner_id }

After receiving: DELETE /message/{messageId}?sessionId={sid}
BrokerMigration / unknown types: ack + continue, never block.
```

### Job Acquire

```
POST {run_service_url}/acquirejob
{ "jobMessageId": "{runner_request_id}", "runnerOS": "Linux", "billingOwnerId": "..." }

→ Full job manifest including:
  steps, env, secrets, jobContainer, serviceContainers,
  plan: { planId, jobId, timelineId }, variables, resources

2-minute window from message receipt to acquire.
```

### Heartbeat

```
POST {run_service_url}/renewjob   (every 60s, from acquire until complete)
{ "planId": "...", "jobId": "..." }
→ { "lockedUntil": "..." }  (always ~10 min in future)
```

### Log Streaming

```
# Before each step:
POST {ServerUrl}/_apis/pipelines/workflows/{planId}/logs/{logId}

# Flush every 1s or 64KB:
POST {ServerUrl}/_apis/pipelines/workflows/{planId}/logs/{logId}
Content-Type: text/plain

2024-01-01T00:00:00.0000000Z line content\n

# Timestamp: RFC3339 with 7 decimal places, space, then the line.
```

### Timeline Updates

```
PATCH {ServerUrl}/_apis/distributedtask/hubs/build/plans/{planId}/timelines/{timelineId}
{
  "value": [{
    "id": "{stepId}",
    "state": 1,          (1=InProgress, 2=Completed)
    "result": null,      (0=Succeeded, 2=Failed, 3=Cancelled)
    "startTime": "...",
    "finishTime": null,
    "name": "Run tests",
    "order": 3
  }],
  "count": 1
}
```

### Job Complete

```
POST {run_service_url}/completejob
{ "planId": "...", "jobId": "...", "conclusion": "success", "outputs": {} }
```

---

## Workflow Commands

Parse from stdout of every step (host and container):

```
::set-output name={n}::{v}   → job output
::set-env name={n}::{v}      → env for subsequent steps + GITHUB_ENV file
::add-path::{v}              → prepend PATH + GITHUB_PATH file
::add-mask::{v}              → redact from all future logs
::debug::{msg}
::warning::{msg}
::error::{msg}
::group::{title}
::endgroup::
::save-state name={n}::{v}
::get-state name={n}::
```

---

## Environment Variables (injected into every job)

```
GITHUB_ACTIONS=true
GITHUB_WORKFLOW, GITHUB_RUN_ID, GITHUB_RUN_NUMBER
GITHUB_JOB, GITHUB_ACTION, GITHUB_ACTOR
GITHUB_REPOSITORY, GITHUB_EVENT_NAME, GITHUB_SHA, GITHUB_REF
GITHUB_WORKSPACE=~/.chimera/work/{runner}/{repo}/{repo}
GITHUB_ENV={workspace}/../_env
GITHUB_PATH={workspace}/../_path
GITHUB_OUTPUT={workspace}/../_output
RUNNER_OS=Linux
RUNNER_ARCH=X64
RUNNER_TEMP=~/.chimera/tmp/{runner}
RUNNER_TOOL_CACHE=~/.chimera/tool-cache
ACTIONS_CACHE_URL=http://localhost:{cache_port}/
ACTIONS_RUNTIME_URL={ServerUrl}
ACTIONS_RUNTIME_TOKEN={token}
```

---

## Docker Resource Contract (for container: and services:)

Per job, created only if job uses `container:` or `services:`:
- Bridge network: `chimera-{runnerId}-{jobId}`
- All containers on that network
- Workspace bind-mounted into job container at `/github/workspace`
- `security_opt: ["no-new-privileges:true"]`, `privileged: false`
- Memory limit from config (default 4GB)

Cleanup order (always, even on panic via `scopeguard::defer!`):
1. Stop containers (SIGTERM → 5s → SIGKILL)
2. `docker rm -v`
3. Remove named volumes created for this job
4. Remove bridge network

Never use `auto_remove: true` (races with log collection).

---

## Signal Handling

```
SIGTERM / SIGINT → graceful_shutdown():
  1. All runners stop accepting new jobs (Draining state)
  2. Wait up to 30s for in-flight jobs to finish
  3. Force-cleanup any remaining Docker resources
  4. DELETE all broker sessions
  5. Exit 0
```

---

## Web UI

No npm. No build step. Single HTML file served inline from the binary (embed with `include_str!`).

### REST API

```
GET  /api/status                  → daemon uptime, version, runner count
GET  /api/runners                 → all runner instances + state + current job
POST /api/runners/{id}/drain      → stop accepting jobs, finish current
POST /api/runners/{id}/restart    → re-register, resume polling
GET  /api/cache/stats             → size used/max, hits, misses, entry count
POST /api/cache/evict             → manual full evict or by key prefix
GET  /api/jobs/history            → last 100 completed jobs (ring buffer)
GET  /api/jobs/{id}/logs          → SSE stream of live log lines
GET  /api/docker/images           → cached images, sizes, last used
POST /api/docker/prune            → trigger image LRU eviction now
```

### UI Pages

**Dashboard** (`/`) — runner cards (name, state, current job, jobs today), cache bar, docker pool size

**Job View** (`/jobs/{id}`) — step list, elapsed time, live log tail via SSE

**Config** (`/config`) — read-only view of loaded config (no secrets)

---

## Testing Strategy

### Principles
- No mocking of internal modules — test real code paths
- Mock only external HTTP APIs via `wiremock`
- Docker tests use real Docker daemon — tagged `#[ignore]` if socket absent, run in CI with Docker available
- Unit tests < 100ms. Integration tests < 10s each.

### Coverage by Module

**broker/**
- Session creation: correct request body, version rejection (400), successful parse
- Long-poll: 202 → None, 200 → Some, BrokerMigration → ack + None
- Retry on transient 500
- Session DELETE on shutdown
- JWT signing: known key + claims → correct decoded token

**job/**
- Acquire parses real captured job manifest fixture (both host-mode and container-mode manifests)
- Heartbeat fires every 60s, cancelled on job completion
- Complete called with correct conclusion

**executor/commands**
- All 10 workflow commands parsed correctly
- `::add-mask::` redacts value from subsequent lines
- `::set-env::` propagates to next step
- Malformed commands ignored without panic

**executor/host**
- `run: echo hello` executes, output captured
- Non-zero exit → step failure
- Workspace created before job, wiped after
- `GITHUB_ENV` file mutations affect subsequent steps
- `GITHUB_OUTPUT` file correctly parsed for outputs

**executor/docker** (requires Docker socket)
- Container starts, step runs via exec, exits
- Network created and destroyed per job
- Workspace bind-mounted correctly
- Cleanup on step failure
- Cleanup when test panics (scopeguard test)
- Image pull skipped when digest unchanged

**executor/services** (requires Docker socket)
- Service container starts before first step
- Accessible by service name from job container
- Stopped and removed after job regardless of outcome

**cache/store**
- Exact key lookup, prefix fallback, miss
- LRU eviction when over max_bytes
- Blob deduplication (two entries, same content, one blob)
- Persist + reload across restart
- Concurrent writes from multiple tasks

**cache/server**
- Full HTTP roundtrip: reserve → upload chunks → commit → GET → download
- 204 on cache miss
- Content-Range upload (chunked)
- Concurrent access (N tokio tasks hitting server simultaneously)

**docker_cache**
- Skip pull when local digest matches
- Pull when digest differs
- LRU eviction removes oldest-used images first

**web/api**
- All endpoints return correct JSON shapes
- Runner state transitions reflected immediately
- SSE stream delivers log lines in order

**daemon**
- N runners all reach Idle state
- One runner erroring doesn't affect others
- Graceful shutdown sequence

---

## Phase Plan

### Phase 1 — Auth + Single Broker + Long-Poll
**Goal:** One runner connects to GitHub broker, polls, logs received job ID, exits cleanly.

- `config.rs` — `~/.chimera/` path layout, toml load, JIT config decode, RSA key extract
- `broker/auth.rs` — JWT signing, OAuth token exchange
- `broker/session.rs` — POST/DELETE /sessions
- `broker/poller.rs` — GET /message loop, 202/200/error/BrokerMigration handling
- `main.rs` — single runner mode, clap CLI (`run`, `install`, `uninstall`, `status`), signal handling skeleton
- `install.rs` — scaffold `~/.chimera/` directory tree, write config template
- **Tests:** wiremock broker tests (session, poll, ack, retry), JWT signing unit test

Done when: `chimera run --config ~/.chimera/config.toml` connects and prints received job messageId.

---

### Phase 2 — Job Acquire + Host Steps + Live Logs
**Goal:** Full job lifecycle for host-mode jobs (no `container:` tag). Live logs in GitHub UI.

- `job/schema.rs` — full manifest types (use real captured fixture as test input)
- `job/acquire.rs` — POST /acquirejob, detect execution mode from manifest
- `job/renew.rs` — heartbeat tokio task
- `job/complete.rs` — POST /completejob
- `executor/host.rs` — bash step execution, stdout/stderr capture, env file mutations
- `executor/commands.rs` — all workflow commands
- `executor/mod.rs` — dispatch logic (host vs container)
- `logs/pager.rs` — batch upload, 1s/64KB flush
- `logs/timeline.rs` — step state PATCH
- `util/workspace.rs` — create/wipe `~/.chimera/work/{runner}/...`
- All `GITHUB_*` + `RUNNER_*` env injection
- **Tests:** host step execution, command parser, log batcher, workspace lifecycle, mock log + timeline APIs

Done when: workflow with only `run:` steps → green checkmarks + live logs in GitHub UI.

---

### Phase 3 — Container Execution + Services
**Goal:** `container:` and `services:` work. Resources always cleaned up.

- `executor/docker.rs` — container execution via bollard, `docker exec` per step
- `executor/services.rs` — service container start/stop, port mapping, DNS
- `executor/resources.rs` — `JobResources` RAII, scopeguard cleanup
- `cache/docker_cache.rs` — inspect-before-pull, digest TTL cache, LRU eviction
- `executor/mod.rs` — wire dispatch to host vs docker based on manifest
- **Tests:** Docker integration (real daemon), cleanup-on-panic, image skip-pull, service DNS

Done when: workflow with `container: ubuntu:latest` and `services: postgres:` runs end-to-end.

---

### Phase 4 — Multi-Runner Daemon
**Goal:** N runners from config all run concurrently, independently.

- `daemon.rs` — spawn N `RunnerInstance` tasks, own shared services
- `runner/mod.rs` — `RunnerInstance` state machine
- `runner/state.rs` — `RunnerState` transitions
- `runner/registry.rs` — `RunnerRegistry`
- Per-runner workspace isolation
- Graceful shutdown (drain all, wait 30s, force-cleanup)
- **Tests:** N runners reach Idle, one error doesn't cascade, shutdown sequence

Done when: config with 3 `[[runner]]` blocks all connect and process jobs concurrently.

---

### Phase 5 — Shared Cache
**Goal:** Rolling LRU cache shared across all runners. `actions/cache@v3` works transparently.

- `cache/store.rs` — content-addressed blobs, blake3, zstd, disk persistence, ref counting
- `cache/manager.rs` — LRU eviction, stats, concurrent access
- `cache/server.rs` — full axum actions/cache compat API
- Wire `ACTIONS_CACHE_URL` into all runner environments
- **Tests:** LRU eviction, blob dedup, persist+reload, concurrent access, full HTTP roundtrip

Done when: second job hits cache; second runner on same machine also hits shared cache.

---

### Phase 6 — Web UI
**Goal:** Dashboard for runner states, cache, live logs. No build tooling required.

- `web/api.rs` — all REST endpoints backed by live state
- `web/ui.rs` — single HTML file, vanilla JS, SSE for live logs
- `/api/jobs/{id}/logs` SSE endpoint
- Runner drain/restart, cache evict, docker prune via UI
- **Tests:** all API endpoints, SSE delivery

Done when: `http://localhost:8080` shows runner states, cache stats, live job logs.

---

### Phase 7 — cgroup v2 Hardening (host mode)
**Goal:** Host execution jobs can't escape their resource limits, process trees fully cleaned up.

- `util/cgroups.rs` — memory limit, CPU quota, guaranteed kill on job end
- Apply cgroup to all host-mode job processes
- Workspace is already isolated via work dir; cgroups add resource enforcement
- **Tests:** memory limit enforced, process tree killed on cgroup teardown

Done when: host mode job that forks background processes has them all killed on job completion.

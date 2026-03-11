# GitHub Actions Runner Protocol

Complete reference for the undocumented wire protocol between a self-hosted
GitHub Actions runner and the GitHub backend services. Derived from reverse
engineering the official runner and re-implementing it in chimera.

---

## Table of Contents

1. [Overview & Key Concepts](#1-overview--key-concepts)
2. [Runner Registration](#2-runner-registration)
3. [Authentication & Token Exchange](#3-authentication--token-exchange)
4. [Broker Session Lifecycle](#4-broker-session-lifecycle)
5. [Job Polling](#5-job-polling)
6. [Job Acquisition](#6-job-acquisition)
7. [Job Manifest Format](#7-job-manifest-format)
8. [Timeline Updates (Step Status)](#8-timeline-updates-step-status)
9. [Log Streaming](#9-log-streaming)
10. [Live Console Feed (WebSocket)](#10-live-console-feed-websocket)
11. [Heartbeat (Job Renewal)](#11-heartbeat-job-renewal)
12. [Job Completion](#12-job-completion)
13. [Workflow Commands](#13-workflow-commands)
14. [Endpoint Reference Table](#14-endpoint-reference-table)
15. [Timestamp Formats](#15-timestamp-formats)
16. [Gotchas & Non-Obvious Behavior](#16-gotchas--non-obvious-behavior)

---

## 1. Overview & Key Concepts

The protocol involves three distinct backend services and two API generations:

| Service | Base URL | Purpose |
|---------|----------|---------|
| **GitHub API** | `https://api.github.com` | Registration only |
| **Pipelines (VSS)** | `https://pipelines.actions.githubusercontent.com/{account_id}` | Legacy: logs, timeline, job completion |
| **Broker** | `https://broker.actions.githubusercontent.com` | V2: session management, job polling |
| **Results (Twirp)** | Varies per job (from manifest variable) | Modern: logs, step status |

**Legacy vs Modern (Results) API**: GitHub is migrating from the VSS/Pipelines API
to the Results Twirp API. The runner must support both. Whether to use Results is
determined per-job by the presence of `system.github.results_endpoint` in the job
manifest's variables. If present, use Twirp. If absent, fall back to VSS.

**Three token types** are in play during a runner's lifecycle:

| Token | How obtained | Used for |
|-------|-------------|----------|
| **Registration token** | User generates via GitHub UI or API | Initial registration only |
| **OAuth token** | JWT exchange with stored RSA key | Runner-level ops (session, polling, acquire) |
| **Job access token** | Embedded in job manifest | Job-level ops (logs, timeline, completion) |

**Runner version**: The runner reports itself as `2.329.0` (constant `RUNNER_VERSION`).
The broker rejects runners with outdated versions — bumping this may be necessary
when GitHub ships breaking changes. The broker protocol version is `3.0.0`
(separate from the runner version).

---

## 2. Runner Registration

Registration is a two-phase process: first authenticate with the GitHub API using
a user-provided registration token, then register the runner itself.

### 2.1 Phase 1: GitHub Authentication

Exchange the registration token for a temporary tenant credential.

```
POST https://api.github.com/actions/runner-registration
Authorization: RemoteAuth {registration_token}
User-Agent: chimera/2.329.0
Content-Type: application/json

{
  "url": "https://github.com/{owner}/{repo}",
  "runner_event": "register"
}
```

**Note**: The `Authorization` header uses the custom `RemoteAuth` scheme, not
`Bearer`. This is the only place this scheme is used.

Response:

```json
{
  "url": "https://pipelines.actions.githubusercontent.com/{account_id}",
  "token": "{temporary_oauth_token}"
}
```

The `url` field is the **tenant URL** (pipelines service base). The `token` is
short-lived and used only for the registration call in phase 2.

### 2.2 Phase 2a: V2 Registration (Preferred)

Chimera tries V2 first. If it fails (404 or other error), falls back to V1.

```
POST https://api.github.com/actions/runners/register
Authorization: Bearer {temporary_oauth_token}
Content-Type: application/json

{
  "url": "https://github.com/{owner}/{repo}",
  "group_id": 1,
  "name": "chimera-0",
  "version": "2.329.0",
  "updates_disabled": true,
  "ephemeral": false,
  "labels": [
    { "name": "self-hosted", "type": "system" },
    { "name": "Linux",       "type": "system" },
    { "name": "X64",         "type": "system" },
    { "name": "my-label",    "type": "custom" }
  ],
  "public_key": "<RSA public key in XML format>"
}
```

Response:

```json
{
  "id": 12345,
  "name": "chimera-0",
  "authorization": {
    "authorization_url": "https://token.actions.githubusercontent.com/oauth2/authorize",
    "server_url": "https://broker.actions.githubusercontent.com",
    "client_id": "{client_uuid}"
  }
}
```

The `server_url` in the authorization block is the **broker URL** for V2 flow.

### 2.3 Phase 2b: V1 Registration (Fallback)

Used when the V2 endpoint is not available (older GitHub Enterprise, etc.).

```
POST {tenant_url}/_apis/distributedtask/pools/1/agents?api-version=6.0-preview
Authorization: Bearer {temporary_oauth_token}
Content-Type: application/json

{
  "name": "chimera-0",
  "version": "2.329.0",
  "osDescription": "Linux X64",
  "enabled": true,
  "status": 0,
  "provisioningState": "Provisioned",
  "authorization": {
    "publicKey": {
      "exponent": "{base64_rsa_exponent}",
      "modulus": "{base64_rsa_modulus}"
    }
  },
  "labels": [
    { "name": "self-hosted", "type": 0 },
    { "name": "Linux",       "type": 0 },
    { "name": "X64",         "type": 0 },
    { "name": "my-label",    "type": 1 }
  ],
  "maxParallelism": 1
}
```

**Key difference from V2**: Labels use numeric types (`0` = System, `1` = User) instead of
string types (`"system"`, `"custom"`). The public key is sent as raw base64 modulus/exponent
instead of XML.

Response:

```json
{
  "id": 12345,
  "name": "chimera-0",
  "authorization": {
    "authorizationUrl": "https://token.actions.githubusercontent.com/oauth2/authorize",
    "clientId": "{client_uuid}"
  },
  "properties": {
    "ServerUrl":   { "$value": "https://pipelines.actions.githubusercontent.com/{account_id}" },
    "ServerUrlV2": { "$value": "https://broker.actions.githubusercontent.com" }
  }
}
```

**Note**: The V1 response uses `$value` wrapper objects for properties, not plain
strings. The broker URL comes from `ServerUrlV2` in properties (defaults to
`https://broker.actions.githubusercontent.com` if missing).

### 2.4 Stored Credentials

After registration, three files are persisted per runner under
`~/.chimera/runners/{name}/`:

**runner.json**:
```json
{
  "agentId": 12345,
  "agentName": "chimera-0",
  "poolId": 1,
  "serverUrl": "https://pipelines.actions.githubusercontent.com/{account_id}",
  "serverUrlV2": "https://broker.actions.githubusercontent.com",
  "gitHubUrl": "https://github.com/owner/repo",
  "workFolder": "_work",
  "useV2Flow": true
}
```

**credentials.json**:
```json
{
  "scheme": "OAuth",
  "clientId": "{client_uuid}",
  "authorizationUrl": "https://token.actions.githubusercontent.com/oauth2/authorize"
}
```

**rsa_params.json**: Full RSA-2048 private key components (d, dp, dq, exponent,
inverseQ, modulus, p, q) in base64. Used to sign JWTs for token exchange.

### 2.5 Unregistration

There is no remote unregister API call. Unregistration is purely local: delete the
runner directory and remove the name from `config.toml`.

---

## 3. Authentication & Token Exchange

Once registered, the runner authenticates using a JWT → OAuth token exchange.
This happens transparently whenever a token is needed (broker session, polling,
job acquisition).

### 3.1 JWT Construction

The JWT is signed with **PS256** (RSASSA-PSS + SHA-256) using the stored RSA
private key.

**Header** (base64url, no padding):
```json
{
  "typ": "JWT",
  "alg": "PS256"
}
```

**Payload** (base64url, no padding):
```json
{
  "sub": "{client_id}",
  "iss": "{client_id}",
  "jti": "{random_uuid_v4}",
  "aud": "{authorization_url}",
  "nbf": 1700000000,
  "iat": 1700000000,
  "exp": 1700000300
}
```

**Timing**: `nbf` and `iat` are set to `now - 30 seconds` (clock skew allowance).
`exp` is `nbf + 5 minutes`. The signature is base64url-encoded without padding.

### 3.2 OAuth Token Exchange

```
POST {authorization_url}
Content-Type: application/x-www-form-urlencoded; charset=utf-8
Accept: application/json

client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer
&client_assertion={signed_jwt}
&grant_type=client_credentials
```

Response:
```json
{
  "access_token": "{oauth_token}",
  "expires_in": 3600
}
```

### 3.3 Token Caching & Refresh

- Tokens are cached until **5 minutes before expiry** (`expires_at > now + 5min`).
- A mutex (`refresh_lock`) serializes concurrent refresh attempts (double-check
  locking pattern).
- On **401 from any API**: the cached token is immediately invalidated, triggering
  a fresh exchange on the next call.
- Default `expires_in` is **3600 seconds** (1 hour) if the field is missing.

---

## 4. Broker Session Lifecycle

Before polling for jobs, the runner creates a session with the broker.

### 4.1 Create Session

```
POST {broker_url}/session
Authorization: Bearer {oauth_token}
Content-Type: application/json
Timeout: 30s

{
  "sessionId": "{fresh_uuid_v4}",
  "ownerName": "hostname (PID: 12345)",
  "agent": {
    "id": 12345,
    "name": "chimera-0",
    "version": "2.329.0",
    "osDescription": "linux aarch64",
    "ephemeral": true,
    "status": 0
  },
  "useFipsEncryption": false
}
```

Response:
```json
{
  "sessionId": "{confirmed_session_id}"
}
```

**Notes**:
- `status: 0` means Online (1 = Offline).
- `ephemeral: true` indicates the runner is transient (single-use per session).
- The broker may return a different `sessionId` than the one you sent (use the
  response value for all subsequent calls).
- A 400 response usually means the runner version is too old.

### 4.2 Delete Session

```
DELETE {broker_url}/session
Authorization: Bearer {oauth_token}
Timeout: 30s
```

- Called on graceful shutdown.
- Both **200** and **404** are acceptable (404 means the session already expired).

---

## 5. Job Polling

The runner uses **long polling** to wait for jobs from the broker.

### 5.1 Poll Message

```
GET {broker_url}/message
    ?sessionId={session_id}
    &status=Online
    &runnerVersion=3.0.0
    &disableUpdate=true
Authorization: Bearer {oauth_token}
Timeout: 55s (client-side)
```

**Note**: The `runnerVersion` query parameter is `3.0.0` (the broker protocol
version), NOT the runner version `2.329.0` used elsewhere.

The server holds the connection for up to ~50 seconds before responding with 202.

| Status | Meaning | Action |
|--------|---------|--------|
| **200** | Job or message available | Parse and handle `BrokerMessage` |
| **202** | No job available | Immediately re-poll |
| **401** | Token expired | Invalidate token, refresh, retry |
| **5xx** | Transient error | Exponential backoff, then retry |

### 5.2 Message Format

```json
{
  "messageId": 123456789,
  "messageType": "RunnerJobRequest",
  "body": "{...json-encoded string...}"
}
```

### 5.3 Message Types

**RunnerJobRequest** — a job is available:
```json
{
  "runner_request_id": "{request_uuid}",
  "run_service_url": "https://pipelines.actions.githubusercontent.com/{account_id}"
}
```

**JobCancellation** — cancel a running job:
```json
{
  "jobId": "{job_uuid}"
}
```

**Unknown / BrokerMigration** — should be acknowledged and ignored.

### 5.4 Acknowledge Job

After receiving a `RunnerJobRequest`, acknowledge it to prevent redelivery:

```
POST {broker_url}/acknowledge
    ?sessionId={session_id}
    &runnerVersion=3.0.0
    &status=Online
    &disableUpdate=true
Authorization: Bearer {oauth_token}
Content-Type: application/json
Timeout: 30s

{
  "runnerRequestId": "{runner_request_id}"
}
```

---

## 6. Job Acquisition

After acknowledging the broker message, acquire the full job manifest from the
run service.

```
POST {run_service_url}/acquirejob
Authorization: Bearer {oauth_token}
Content-Type: application/json
Timeout: 30s

{
  "jobMessageId": "{runner_request_id}",
  "runnerOS": "Linux"
}
```

Response: the full job manifest (see next section).

**Critical**: This must happen within **~2 minutes** of receiving the broker
message, or the job assignment expires.

**Token**: This uses the runner's **OAuth token** (not the job access token, which
doesn't exist yet — it comes inside the manifest response).

---

## 7. Job Manifest Format

The manifest response from `/acquirejob` is in GitHub's internal **TemplateToken**
encoding. It must be normalized to plain JSON before deserialization.

### 7.1 TemplateToken Types

The raw manifest encodes values as typed tokens:

| Type | Name | Format | Normalized to |
|------|------|--------|--------------|
| 0 | String | `{"type": 0, "lit": "value"}` | `"value"` |
| 1 | Sequence | `{"type": 1, "seq": [...]}` | `[...]` |
| 2 | Mapping | `{"type": 2, "map": [{"Key": {...}, "Value": {...}}]}` | `{...}` |
| 3 | Expression | `{"type": 3, "expr": "success()"}` | `"${{ success() }}"` |
| 5 | Boolean | `{"type": 5, "bool": true}` | `true` |
| 6 | Number | `{"type": 6, "num": 42}` | `42` |
| 7 | Null | `{"type": 7}` | `null` |

**Note**: There is no type 4 — it's skipped in the enum.

**Sequence subtlety**: A type-1 token can be either a literal array OR a template
string. If any element in the sequence is an expression token (type 3), the entire
sequence is concatenated into a single string (preserving `${{ }}` wrappers).
Otherwise it's treated as a plain array.

**Mapping keys**: The `map` array uses `Key`/`Value` (capital K/V) or `key`/`value`
(lowercase) — the normalizer handles both.

### 7.2 PipelineContextData Types

Context data (the `contextData` field) uses a different encoding with short field
names:

| Type | Name | Format | Normalized to |
|------|------|--------|--------------|
| 0 | String | `{"t": 0, "s": "value"}` | `"value"` |
| 1 | Array | `{"t": 1, "a": [...]}` | `[...]` |
| 2 | Dictionary | `{"t": 2, "d": [{"k": "key", "v": {...}}]}` | `{...}` |
| 3 | Bool | `{"t": 3, "b": true}` | `true` |
| 4 | Number | `{"t": 4, "n": 42}` | `42` |

**Key difference from TemplateToken**: uses `t` instead of `type`, and field names
are single letters (`s`, `a`, `d`, `k`, `v`, `b`, `n`).

### 7.3 Normalized Manifest Structure

After normalization, the manifest looks like:

```json
{
  "plan": {
    "planId": "{plan_uuid}",
    "jobId": "{job_uuid}",
    "timelineId": "{timeline_uuid}"
  },
  "steps": [
    {
      "id": "{step_uuid}",
      "displayName": "Run echo hello",
      "reference": {
        "name": "script",
        "type": "script",
        "ref": null,
        "path": null,
        "repositoryType": null,
        "image": null
      },
      "inputs": {
        "script": "echo hello"
      },
      "condition": "${{ success() }}",
      "timeoutInMinutes": 360,
      "continueOnError": false,
      "order": 1,
      "environment": {
        "MY_VAR": "value"
      }
    }
  ],
  "variables": {
    "system.github.token": {
      "value": "{github_token}",
      "isSecret": true
    },
    "system.github.results_endpoint": {
      "value": "https://results.actions.githubusercontent.com",
      "isSecret": false
    }
  },
  "resources": {
    "endpoints": [
      {
        "name": "SystemVssConnection",
        "url": "https://pipelines.actions.githubusercontent.com/{account_id}",
        "authorization": {
          "scheme": "OAuth",
          "parameters": {
            "AccessToken": "{job_access_token}"
          }
        },
        "data": {
          "PipelinesServiceUrl": "https://pipelines.actions.githubusercontent.com/{account_id}",
          "FeedStreamUrl": "https://feed.actions.githubusercontent.com/{account_id}"
        }
      }
    ]
  },
  "contextData": {
    "github": {
      "repository": "owner/repo",
      "sha": "abc123def",
      "ref": "refs/heads/main",
      "workflow": ".github/workflows/ci.yml",
      "run_id": "123456789",
      "run_number": "42",
      "job": "build",
      "action": "__run",
      "actor": "username",
      "event_name": "push"
    }
  },
  "jobContainer": {
    "image": "ubuntu:22.04",
    "environment": { "DEBIAN_FRONTEND": "noninteractive" },
    "options": "--network-alias=job",
    "volumes": ["/host:/container"],
    "ports": ["8080:8080"],
    "credentials": {
      "username": "user",
      "password": "{secret}"
    }
  },
  "serviceContainers": [
    {
      "image": "postgres:15",
      "alias": "postgres",
      "environment": { "POSTGRES_PASSWORD": "secret" },
      "ports": ["5432:5432"]
    }
  ],
  "mask": ["secret_value_1", "secret_value_2"],
  "fileTable": ["path/to/action"]
}
```

### 7.4 Step Reference Types

| `type` value | Meaning | Key fields |
|-------------|---------|------------|
| `"script"` | A `run:` step | `inputs.script` contains the shell script |
| `"repository"` | An action from a repo | `name` = `owner/action`, `ref` = git ref, `path` = subdir |
| `"containerregistry"` | A Docker action | `image` = container image |

### 7.5 Key Manifest Fields

**SystemVssConnection endpoint** is the most important resource. It contains:
- `url` — the pipelines service base URL (used for legacy APIs and completejob)
- `authorization.parameters.AccessToken` — the **job access token**
- `data.PipelinesServiceUrl` — explicit pipelines URL (may differ from `url`)
- `data.FeedStreamUrl` — WebSocket URL for live console streaming

**`system.github.results_endpoint`** variable — if present, enables the modern
Results Twirp API for this job. This is how you know whether to use legacy VSS
or modern Results for timeline/log operations.

### 7.6 Service Containers Normalization

GitHub sends service containers in two possible formats:

1. **Plain array** of container specs (already normalized)
2. **Template token mapping** (type=2) where each key is the service alias:
   ```json
   {"type": 2, "map": [{"Key": {"type": 0, "lit": "redis"}, "Value": {container_spec_token}}]}
   ```
   This gets normalized to an array, with the map key injected as the `alias` field.

### 7.7 Display Name Extraction

Step display names live in `displayNameToken.lit` (a string template token), not
in a plain `displayName` field. The normalizer extracts the literal value. Falls
back to `name` field, then to `"(unnamed step)"`.

---

## 8. Timeline Updates (Step Status)

The runner reports step progress to GitHub so the UI shows real-time status.
There are two APIs depending on legacy/modern mode.

### 8.1 Legacy VSS API

```
PATCH {pipelines_url}/_apis/pipelines/workflows/{plan_id}/timelines/{timeline_id}/records
Authorization: Bearer {job_access_token}
Content-Type: application/json
Timeout: 15s

{
  "value": [
    {
      "id": "{step_uuid}",
      "state": 1,
      "result": null,
      "startTime": "2024-01-15T10:30:00.0000000Z",
      "finishTime": null,
      "name": "Run tests",
      "order": 1,
      "log": { "id": 42 }
    }
  ],
  "count": 1
}
```

**State values** (`TimelineState`):

| Value | Meaning |
|-------|---------|
| 1 | InProgress |
| 2 | Completed |

**Result values** (`TimelineResult`):

| Value | Meaning |
|-------|---------|
| null | In progress (no result yet) |
| 0 | Succeeded |
| 2 | Failed |
| 3 | Cancelled |
| 4 | Skipped |

**Note**: Result value 1 is skipped (there is no value 1).

The `log.id` field links the timeline record to the corresponding log resource
created via `POST .../logs`.

### 8.2 Results Twirp API (Modern)

```
POST {results_url}/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate
Authorization: Bearer {job_access_token}
Content-Type: application/json
Timeout: 15s

{
  "steps": [
    {
      "external_id": "{step_uuid}",
      "number": 1,
      "name": "Run tests",
      "status": 1,
      "started_at": "2024-01-15T10:30:00.000Z",
      "completed_at": null,
      "conclusion": 0
    }
  ],
  "change_order": 1,
  "workflow_run_backend_id": "{plan_id}",
  "workflow_job_run_backend_id": "{job_id}"
}
```

**Status values** (`ResultsStatus`):

| Value | Meaning |
|-------|---------|
| 0 | Pending |
| 1 | InProgress |
| 3 | Completed |

**Note**: Value 2 is skipped.

**Conclusion values** (`ResultsConclusion`):

| Value | Meaning |
|-------|---------|
| 0 | Unknown (in progress) |
| 2 | Success |
| 3 | Failure |
| 4 | Cancelled |
| 5 | Skipped |

**Note**: Value 1 is skipped.

**`change_order`**: An incrementing counter per job. Each call must use a strictly
higher value than the previous. The runner uses an `AtomicI64` starting at 0,
incrementing by 1 for each update.

---

## 9. Log Streaming

Log output for each step is sent to GitHub in real time. The mechanism differs
between legacy and modern APIs.

### 9.1 Log Line Format

All log lines are timestamped:
```
2024-01-15T10:30:00.0000000Z Hello world
```

The timestamp uses **RFC3339 with 7 decimal places** (100-nanosecond precision),
followed by a space, then the content.

### 9.2 Legacy VSS API

Two-step process: create a log resource, then stream lines to it.

**Create log:**
```
POST {pipelines_url}/_apis/pipelines/workflows/{plan_id}/logs
Authorization: Bearer {job_access_token}
Content-Type: application/json

{
  "path": "logs/{step_name}"
}
```

Response:
```json
{
  "id": 42
}
```

**Upload log lines:**
```
POST {pipelines_url}/_apis/pipelines/workflows/{plan_id}/logs/{log_id}
Authorization: Bearer {job_access_token}
Content-Type: application/octet-stream
Timeout: 15s

2024-01-15T10:30:00.0000000Z Line 1
2024-01-15T10:30:00.1000000Z Line 2
```

The body is raw text (newline-separated timestamped lines), not JSON.

**Flush strategy**: Buffer is flushed every **1 second** or when it reaches
**64 KB**, whichever comes first. This balances real-time visibility with API
call overhead.

### 9.3 Results Twirp API (Modern) — Azure Append Blob

The modern API uses Azure Blob Storage append blobs for streaming. The flow is:

#### Step 1: Get a signed URL

```
POST {results_url}/twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL
Authorization: Bearer {job_access_token}
Content-Type: application/json

{
  "workflow_run_backend_id": "{plan_id}",
  "workflow_job_run_backend_id": "{job_id}",
  "step_backend_id": "{step_id}"
}
```

Response:
```json
{
  "logs_url": "https://{storage_account}.blob.core.windows.net/...",
  "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE"
}
```

#### Step 2: Create the append blob

```
PUT {logs_url}
x-ms-blob-type: AppendBlob
Content-Length: 0
```

Response: **201 Created**

#### Step 3: Append blocks incrementally

As log lines come in, buffer and flush them as append blocks:

```
PUT {logs_url}&comp=appendblock
Content-Length: {byte_count}

2024-01-15T10:30:00.0000000Z Line 1
2024-01-15T10:30:00.1000000Z Line 2
```

Response: **201 Created**

Same flush strategy as legacy: every 1 second or 64 KB.

#### Step 4: Post metadata (after each flush)

Notify GitHub that new data is available:

```
POST {results_url}/twirp/results.services.receiver.Receiver/CreateStepLogsMetadata
Authorization: Bearer {job_access_token}
Content-Type: application/json

{
  "workflow_run_backend_id": "{plan_id}",
  "workflow_job_run_backend_id": "{job_id}",
  "step_backend_id": "{step_id}",
  "uploaded_at": "2024-01-15T10:30:00.000Z",
  "line_count": 42
}
```

**Note**: `uploaded_at` uses Results timestamp format (3 decimal places), and
`line_count` is the **total** cumulative line count, not the count of new lines.

#### Step 5: Seal the blob (when step completes)

```
PUT {logs_url}&comp=seal
Content-Length: 0
```

Sealing prevents further appends and signals that the log is complete.

### 9.4 Job-Level Log Upload

At the end of a job, the complete job log can be uploaded as a single blob:

```
POST {results_url}/twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL
Authorization: Bearer {job_access_token}

{ "workflow_run_backend_id": "{plan_id}", "workflow_job_run_backend_id": "{job_id}" }
```

Then create, append+seal in one shot:
```
PUT {logs_url}&comp=appendblock&seal=true
x-ms-blob-sealed: true
Content-Length: {byte_count}

{full log content}
```

Followed by metadata:
```
POST {results_url}/twirp/results.services.receiver.Receiver/CreateJobLogsMetadata

{
  "workflow_run_backend_id": "{plan_id}",
  "workflow_job_run_backend_id": "{job_id}",
  "uploaded_at": "2024-01-15T10:30:00.000Z",
  "line_count": 500
}
```

---

## 10. Live Console Feed (WebSocket)

Optional real-time log streaming to the GitHub UI via WebSocket. This powers the
"live" view when you click on a running step.

### 10.1 Connection

The `FeedStreamUrl` from the SystemVssConnection endpoint's `data` field is an
HTTPS URL — convert to WSS for WebSocket:

```
wss://feed.actions.githubusercontent.com/{account_id}
```

Connect with headers:
```
Authorization: Bearer {job_access_token}
Connection: Upgrade
Upgrade: websocket
Sec-WebSocket-Version: 13
Sec-WebSocket-Key: {random_key}
```

### 10.2 Message Format

Lines are batched (up to 100 per message) and sent every **500ms**:

```json
{
  "Count": 3,
  "Value": ["line 1 content", "line 2 content", "line 3 content"],
  "StepId": "{step_uuid}",
  "StartLine": 1
}
```

**`StartLine`** is 1-based and increments with each batch for a given step.

Lines are truncated to **1024 characters** before sending.

### 10.3 Connection Management

- A background task drains the read side of the WebSocket to ensure ping/pong
  frames are handled automatically (keeps connection alive).
- If a send fails, the feed is marked as dead and further lines are silently
  dropped (doesn't affect log upload via the primary APIs).
- On step/job completion, remaining buffered lines are flushed and the WebSocket
  is closed cleanly.

---

## 11. Heartbeat (Job Renewal)

While a job is executing, the runner must periodically renew the job lock.

```
POST {run_service_url}/renewjob
Authorization: Bearer {job_access_token}
Content-Type: application/json
Timeout: 30s

{
  "planId": "{plan_id}",
  "jobId": "{job_id}"
}
```

Response:
```json
{
  "lockedUntil": "2024-01-15T11:40:00Z"
}
```

**Interval**: Every **60 seconds**, starting immediately after job acquisition.
The lock typically extends for ~10 minutes, so missing a few heartbeats is not
immediately fatal, but consistent failure will cause the job to time out.

The heartbeat runs as a background task concurrent with job execution and is
cancelled when the job completes.

---

## 12. Job Completion

After all steps finish (or the job is cancelled), report the final result.

```
POST {server_url}/completejob
Authorization: Bearer {job_access_token}
Content-Type: application/json
Timeout: 30s

{
  "planId": "{plan_id}",
  "jobId": "{job_id}",
  "conclusion": "succeeded",
  "outputs": {
    "result": "success"
  },
  "stepResults": [
    {
      "external_id": "{step_uuid}",
      "number": 1,
      "name": "Run tests",
      "status": "Completed",
      "conclusion": "Succeeded",
      "started_at": "2024-01-15T10:30:00.000Z",
      "completed_at": "2024-01-15T10:31:00.000Z"
    }
  ]
}
```

**Important**: The `conclusion` field only accepts **`"succeeded"`** or
**`"failed"`**. There is no `"cancelled"` value — cancelled jobs must report
`"failed"`. GitHub tracks the cancellation status server-side and displays the
correct badge in the UI regardless.

**`server_url`**: This is the pipelines URL from the SystemVssConnection endpoint
(not the run_service_url used for acquire/renew). The distinction matters.

---

## 13. Workflow Commands

The runner parses special command strings from step stdout to implement GitHub
Actions' workflow command interface.

**Format**: `::command-name param=value,param2=value2::message`

| Command | Parameters | Effect |
|---------|-----------|--------|
| `::set-output name=X::Y` | name | Sets step output `X` to `Y` |
| `::set-env name=X::Y` | name | Sets env var `X=Y` for subsequent steps |
| `::add-path::X` | — | Prepends `X` to `PATH` for subsequent steps |
| `::add-mask::X` | — | Redacts `X` from all future log output |
| `::debug::X` | — | Debug-level log annotation |
| `::warning::X` | — | Warning annotation (shown in PR) |
| `::error::X` | — | Error annotation (causes step to fail) |
| `::group::X` | — | Start a collapsible group named `X` |
| `::endgroup::` | — | End the current group |
| `::save-state name=X::Y` | name | Save state for action pre/post lifecycle |

Parameters are comma-separated key=value pairs between the command name and `::`.

---

## 14. Endpoint Reference Table

| Purpose | Method | URL | Auth Token |
|---------|--------|-----|-----------|
| Registration auth | POST | `https://api.github.com/actions/runner-registration` | `RemoteAuth {reg_token}` |
| V2 register | POST | `https://api.github.com/actions/runners/register` | Bearer (temp OAuth) |
| V1 register | POST | `{tenant}/_apis/distributedtask/pools/1/agents?api-version=6.0-preview` | Bearer (temp OAuth) |
| Create session | POST | `{broker}/session` | Bearer (OAuth) |
| Delete session | DELETE | `{broker}/session` | Bearer (OAuth) |
| Poll message | GET | `{broker}/message?sessionId=...&status=Online&runnerVersion=3.0.0&disableUpdate=true` | Bearer (OAuth) |
| Acknowledge | POST | `{broker}/acknowledge?sessionId=...&runnerVersion=3.0.0&status=Online&disableUpdate=true` | Bearer (OAuth) |
| Acquire job | POST | `{run_service}/acquirejob` | Bearer (OAuth) |
| Renew job | POST | `{run_service}/renewjob` | Bearer (job token) |
| Complete job | POST | `{server}/completejob` | Bearer (job token) |
| Create log (VSS) | POST | `{pipelines}/_apis/pipelines/workflows/{plan}/logs` | Bearer (job token) |
| Upload log lines (VSS) | POST | `{pipelines}/_apis/pipelines/workflows/{plan}/logs/{id}` | Bearer (job token) |
| Update timeline (VSS) | PATCH | `{pipelines}/_apis/pipelines/workflows/{plan}/timelines/{tid}/records` | Bearer (job token) |
| Get step log URL | POST | `{results}/twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL` | Bearer (job token) |
| Get job log URL | POST | `{results}/twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL` | Bearer (job token) |
| Step log metadata | POST | `{results}/twirp/results.services.receiver.Receiver/CreateStepLogsMetadata` | Bearer (job token) |
| Job log metadata | POST | `{results}/twirp/results.services.receiver.Receiver/CreateJobLogsMetadata` | Bearer (job token) |
| Update steps | POST | `{results}/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate` | Bearer (job token) |
| Create blob | PUT | `{signed_url}` | URL-signed (no header) |
| Append block | PUT | `{signed_url}&comp=appendblock` | URL-signed (no header) |
| Seal blob | PUT | `{signed_url}&comp=seal` | URL-signed (no header) |
| Live feed | WSS | `wss://feed.actions.githubusercontent.com/{id}` | Bearer (job token, in header) |

---

## 15. Timestamp Formats

The protocol uses three different timestamp formats depending on the API:

| API | Format | Example | Precision |
|-----|--------|---------|-----------|
| Timeline (VSS) | RFC3339 + 7 decimals | `2024-01-15T10:30:00.0000000Z` | 100 ns |
| Log lines | RFC3339 + 7 decimals | `2024-01-15T10:30:00.0000000Z` | 100 ns |
| Results (Twirp) | RFC3339 + 3 decimals | `2024-01-15T10:30:00.000Z` | 1 ms |

Using the wrong precision for an API may cause silent failures or incorrect
display in the GitHub UI.

---

## 16. Gotchas & Non-Obvious Behavior

### Token confusion

Three different tokens are used at different stages. Using the wrong one produces
a 401 that can be hard to debug:

- **OAuth token** (from JWT exchange): broker session, polling, acknowledge, acquire job
- **Job access token** (from manifest): logs, timeline, renew, complete
- **Registration token** (user-provided): only for initial registration

### `completejob` uses `server_url`, not `run_service_url`

The `acquirejob` and `renewjob` calls use the `run_service_url` from the broker
message. But `completejob` uses the `server_url` from the SystemVssConnection
endpoint in the manifest. These are often the same host, but not always.

### Cancelled → "failed"

The `completejob` API does not accept `"cancelled"` as a conclusion. Cancelled
jobs must report `"failed"`. GitHub handles the actual status display correctly
based on its own cancellation tracking.

### Version numbers matter

- `RUNNER_VERSION` (`2.329.0`) in session/registration bodies
- `BROKER_PROTOCOL_VERSION` (`3.0.0`) in poll/ack query strings
- These are separate values. Confusing them causes 400 errors.

### Legacy VSS log upload is `Content-Type: application/octet-stream`

Unlike every other API call (which is JSON), the VSS log line upload sends raw
text bytes with `application/octet-stream`.

### Template token type gaps

Both TemplateToken and PipelineContextData have gaps in their type enums (no type
4 in TemplateToken, different semantics for types between the two systems). Don't
assume sequential numbering.

### Results API is Twirp, not REST

The Results API uses [Twirp](https://twitchtv.github.io/twirp/) (Protobuf over
HTTP/JSON). All calls are POST with JSON bodies to long
`/twirp/package.Service/Method` URLs. There are no path parameters or query
strings.

### Blob operations don't use Authorization headers

The signed blob URLs contain embedded SAS tokens. Do NOT send an `Authorization`
header with blob operations — it will conflict with the URL signature and produce
a 403.

### Azure-specific headers

When `blob_storage_type` is `"BLOB_STORAGE_TYPE_AZURE"`:
- `PUT` to create blob must include `x-ms-blob-type: AppendBlob`
- `PUT` to append must include `Content-Length`
- Combined append+seal uses `x-ms-blob-sealed: true` header

### Service containers can arrive as a template-token map

Service containers (`jobServiceContainers` or `serviceContainers` in the raw
manifest) may be a type-2 mapping token keyed by alias name, not a plain array.
The normalizer converts this to an array and injects the alias.

### Session ID from response

The broker may return a different session ID than the one you proposed in the
create-session request. Always use the response value.

### Poll timeout layering

The client sets a 55-second timeout. The server holds for up to 50 seconds.
The 5-second gap ensures the client timeout fires after the server response,
not before.

### Log metadata is posted after every flush

In Results mode, `CreateStepLogsMetadata` is called after each flush (not just at
the end). This is what triggers the GitHub UI to refresh the log view. Without it,
logs appear only after the step completes.

### `FeedStreamUrl` is HTTPS, connect as WSS

The `FeedStreamUrl` in the manifest data comes as an `https://` URL. It must be
converted to `wss://` for the WebSocket connection.

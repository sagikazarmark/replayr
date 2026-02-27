# Replayr — Project Specification

**API Record/Replay/Simulation for AI Applications**

*Version 0.1 — Draft*
*February 2026*

---

## 1. Vision

Replayr is a Rust-based API record/replay/simulation tool for testing any HTTP-based integration, with first-class support for AI/LLM APIs. It acts as a proxy that records real API interactions, then replays them deterministically in tests — eliminating flaky tests, reducing API costs, and enabling sophisticated failure scenario testing.

It works for **any API** (REST, webhooks, third-party services), but goes further for LLM providers (Anthropic, OpenAI, etc.) with streaming-aware recording, token tracking, and provider-specific response templates.

**What makes Replayr different from Hoverfly, WireMock, and MockServer:**

- **Runs everywhere** — Same Rust core compiles to a native CLI binary, a Docker container, a Cloudflare Worker (via WASM), and eventually an embeddable library. Record in production/staging, replay in CI, author scenarios in the cloud UI.
- **LLM superpowers** — First-class support for SSE streaming, tool use / function calling flows, non-deterministic response matching, and token/cost tracking. But these are additive — the core is a general-purpose API simulation tool.
- **Two entry points** — Record real interactions for app-specific testing, *or* use the built-in scenario library for instant error-handling tests without any recording.
- **Compiled scenarios** — Self-contained, single-file test fixtures with all responses inlined. No dangling references, no extra downloads. Commit them to your repo or pull them from a hosted instance.
- **Visual scenario builder** — A TanStack Start UI for assembling test scenarios, reviewing recorded traffic, and browsing the scenario library.

---

## 2. Terminology

| Term | Definition |
|---|---|
| **Cassette** | A recorded collection of request/response pairs from real API traffic. Raw material for building scenarios. Internal to the recording workflow — users typically don't distribute cassettes directly. |
| **Scenario** | A test fixture that defines what Replayr should do: which responses to serve, in what order, with what faults. Can be *authored* (references cassette interactions by ID) or *compiled* (self-contained with all responses inlined). |
| **Compiled Scenario** | The distribution format. A single self-contained file with all responses, matchers, faults, and timing baked in. No external references. This is what you commit to your repo, download in CI, or share with teammates. |
| **Interaction** | A single request/response pair. For streaming responses, includes all SSE chunks with timing. |
| **Matcher** | A rule that determines whether an incoming request maps to a scenario step. |
| **Mode** | The operating state of the proxy: Record, Replay, Passthrough, or Spy. |
| **Scenario Library** | A built-in collection of ready-to-use compiled scenarios for common patterns (rate limits, auth errors, timeouts, etc.) organized by provider. No recording needed — just pick, customize, and test. |
| **Fixture Pack** | A downloadable bundle of multiple compiled scenarios, versioned and cacheable, for use in CI/local testing. |

---

## 3. Architecture

### 3.1 Core Design Principle: One Rust Core, Multiple Runtimes

```
┌─────────────────────────────────────────────┐
│              replayr-core (Rust)             │
│                                             │
│  ┌─────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ Proxy   │ │ Matcher  │ │ Scenario     │ │
│  │ Engine  │ │ Engine   │ │ Engine       │ │
│  └─────────┘ └──────────┘ └──────────────┘ │
│  ┌─────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ Storage │ │ Cassette │ │ Admin API    │ │
│  │ Abstrac.│ │ Format   │ │ (REST)       │ │
│  └─────────┘ └──────────┘ └──────────────┘ │
└──────────┬──────────┬──────────┬────────────┘
           │          │          │
     ┌─────┴──┐  ┌────┴───┐  ┌──┴──────────┐
     │  CLI   │  │ WASM   │  │  Library    │
     │ Binary │  │ Worker │  │  (crate)    │
     └────────┘  └────────┘  └─────────────┘
         │           │              │
     Native OS   Cloudflare     Embedded in
     + Docker    Workers        test code
```

### 3.2 Proxy Modes

Replayr supports two interception strategies, selectable per deployment:

**Reverse Proxy (base_url swap):**
- Client points `base_url` to Replayr instead of the real API
- Replayr forwards to the real upstream (in Record/Spy mode) or responds from cassette (in Replay mode)
- Works everywhere, including Cloudflare Workers (no CONNECT tunneling needed)
- Simplest integration: change one env var (`ANTHROPIC_BASE_URL=http://localhost:9090`)

**Forward Proxy (HTTP_PROXY):**
- Client sets `HTTP_PROXY` / `HTTPS_PROXY` env var
- Replayr intercepts all outbound traffic transparently
- Requires TLS termination (MITM) for HTTPS — CLI/Docker only
- More transparent: no code changes needed in the application under test

**Recommendation for MVP:** Start with reverse proxy only. It works across all runtimes and is simpler. Add forward proxy support in a later milestone for the CLI/Docker targets.

### 3.3 Operating Modes

| Mode | Behavior | Recording |
|---|---|---|
| **Proxy** (default) | Forwards requests to upstream, returns responses | Always captures to ring buffer. Optional auto-record to disk (toggle via CLI flag, inspector UI, or admin API) |
| **Serve** | Responds from a compiled scenario. Never hits real API unless `unmatched: passthrough` | Ring buffer active. Can save/record for debugging |
| **Spy** | Tries scenario first; on miss, forwards to upstream. Optionally records unmatched | Ring buffer + optional record of new interactions |

Note: "Record" is not a separate mode — it's a persistence toggle available in any mode. Traffic is always captured in memory; recording just means writing it to disk.

### 3.4 Storage Abstraction

```
trait ReplayrStorage {
    async fn save_cassette(id, cassette) -> Result<()>;
    async fn load_cassette(id) -> Result<Cassette>;
    async fn list_cassettes(filter) -> Result<Vec<CassetteMeta>>;
    async fn save_scenario(id, scenario) -> Result<()>;
    // ... etc
}
```

**Implementations:**

| Backend | Use case | Cassette body storage | Metadata/index |
|---|---|---|---|
| **Filesystem** | CLI, Docker, local dev | Files on disk | SQLite |
| **Cloudflare** | Hosted version | R2 (S3-compatible) | D1 (SQLite edge) |
| **In-memory** | Embedded tests, ephemeral | Memory | Memory |

Cassette bodies (which can be large, especially with streaming responses) are stored in blob storage (files / R2), while metadata, indexes, and scenario definitions live in SQLite / D1.

---

## 4. Cassette Format (Recording Format)

### 4.1 Role

Cassettes are the **raw recording format** — they capture everything from a recording session. Think of them as raw footage before editing. Users typically don't distribute cassettes directly; instead, they build scenarios from cassettes and compile them into self-contained fixtures.

The flow is: **Record → Cassette → Author scenario → Compile → Distribute compiled scenario**

However, cassettes are also useful for:
- Reviewing what was actually recorded before building scenarios
- Sharing raw recordings between team members
- Importing/exporting to other tools (HAR, VCR, Hoverfly)

### 4.2 Schema (Draft)

```jsonc
{
  "replayr_version": "1",
  "cassette": {
    "id": "uuid",
    "name": "anthropic-chat-basic",
    "description": "Basic Claude chat completion, streaming",
    "created_at": "2026-02-25T10:00:00Z",
    "tags": ["anthropic", "streaming", "claude-sonnet"],
    "upstream": "https://api.anthropic.com"
  },
  "interactions": [
    {
      "id": "uuid",
      "recorded_at": "2026-02-25T10:00:01Z",
      "request": {
        "method": "POST",
        "path": "/v1/messages",
        "headers": {
          "content-type": "application/json",
          "x-api-key": "REDACTED",          // auto-redacted
          "anthropic-version": "2023-06-01"
        },
        "body": {
          // original JSON body preserved
          "model": "claude-sonnet-4-20250514",
          "max_tokens": 1024,
          "stream": true,
          "messages": [
            {"role": "user", "content": "Hello, Claude!"}
          ]
        }
      },
      "response": {
        "status": 200,
        "headers": {
          "content-type": "text/event-stream",
          "request-id": "req_abc123"
        },
        // For streaming responses: array of chunks with timing
        "streaming": true,
        "chunks": [
          {
            "delay_ms": 0,
            "data": "event: message_start\ndata: {\"type\":\"message_start\"...}\n\n"
          },
          {
            "delay_ms": 45,
            "data": "event: content_block_delta\ndata: {\"type\":\"content_block_delta\"...}\n\n"
          }
          // ... more chunks
        ],
        // For non-streaming: regular body
        // "body": { ... }
      },
      "metadata": {
        "provider": "anthropic",
        "model": "claude-sonnet-4-20250514",
        "input_tokens": 12,
        "output_tokens": 85,
        "total_tokens": 97,
        "estimated_cost_usd": 0.00123,
        "latency_ms": 1250,
        "latency_to_first_chunk_ms": 180
      },
      "matcher": {
        // How this interaction should be matched during replay
        // null = use scenario/global defaults
        "strategy": "default"
      }
    }
  ]
}
```

### 4.3 Sensitive Data Handling

During recording, Replayr automatically redacts:
- `Authorization` / `x-api-key` / `api-key` headers → `"REDACTED"`
- Bearer tokens → `"REDACTED"`
- Configurable additional patterns (regex-based)

Users can also define custom redaction rules for request/response bodies (e.g., PII in prompts or completions).

### 4.4 Export Adapters

| Format | Export | Import | Notes |
|---|---|---|---|
| **HAR** | ✅ | ✅ | Streaming collapses to single response body; timing approximated |
| **VCR.py (YAML)** | ✅ | ✅ | For Python ecosystem compatibility |
| **Hoverfly JSON** | ✅ | ✅ | Loses streaming chunk timing |
| **cURL** | ✅ | ❌ | Export individual interactions as curl commands |
| **OpenAPI stub** | ✅ | ❌ | Generate OpenAPI spec from recorded traffic (later milestone) |

---

## 5. Matching Engine

### 5.1 Philosophy

Start simple, evolve to smart. The matching engine determines which recorded interaction to serve for an incoming request during Replay/Spy mode.

### 5.2 Matching Strategy (Phased)

**Phase 1 — Exact matching (MVP):**
- Match on: HTTP method + path + normalized body (sorted JSON keys)
- Headers ignored by default (most are ephemeral)
- First match wins (ordered by recording sequence)

**Phase 2 — CEL-based matchers:**
- Full CEL expressions for matching: `request.method == "POST" && request.body.model.startsWith("claude")`
- Reuses the same CEL engine as filtering and intercept — one language everywhere
- Per-field convenience matchers (glob, regex) as syntactic sugar that compiles to CEL internally
- Body matchers: JSON field access via CEL dot notation (`request.body.messages.size() > 0`)

**Phase 3 — LLM-aware smart matching:**
- Provider-specific presets: "Anthropic chat" matcher knows to match on `model` + `messages` content, ignore `max_tokens`, `temperature`, `stream`
- Fuzzy prompt matching: normalize whitespace, ignore trailing punctuation
- Semantic similarity matching (optional, for advanced use): embed prompts and match on cosine similarity above a threshold

### 5.3 Multiple Match Resolution

When multiple interactions match a request:
1. **Sequence mode** (default for scenarios): serve the next unserved match in order
2. **First match**: always return the first matching interaction
3. **Round-robin**: cycle through matches
4. **Random**: pick randomly (for chaos testing)

---

## 6. Scenario Engine

Scenarios are the primary unit of testing in Replayr. They define what the proxy does: which responses to serve, in what order, with what faults.

### 6.1 Two Entry Points

**Path A: Record → Build → Compile (app-specific testing)**
```
Record real API calls  →  Cassette (raw)  →  Author scenario (pick interactions)  →  Compile  →  Self-contained fixture
```
Use when: You need to test with your app's actual prompts, responses, and data shapes.

**Path B: Scenario Library → Customize → Use (generic testing)**
```
Browse built-in templates  →  Pick a pattern  →  Customize (tweak responses, timing)  →  Ready to use
```
Use when: You want to test error handling, retry logic, edge cases — without recording anything first. Covers ~60-70% of testing needs.

### 6.2 Authored vs. Compiled Scenarios

**Authored scenario** (development-time format):
- References cassette interactions by ID
- DRY — multiple scenarios can share the same recorded interactions
- Used in the UI and during scenario development
- Not distributed — this is a "source" format

**Compiled scenario** (distribution format):
- Self-contained single file, all responses inlined
- Only includes the interactions actually needed (tree-shaken from cassettes)
- No external references, no cassette dependencies
- This is what you commit, download, share, and run

```jsonc
// Compiled scenario — everything you need in one file
{
  "replayr_version": "1",
  "format": "compiled",
  "scenario": {
    "id": "uuid",
    "name": "rate-limit-after-3-requests",
    "description": "Simulate Anthropic rate limiting after 3 successful calls",
    "provider_hint": "anthropic"    // optional, for UI grouping
  },
  "steps": [
    {
      "match": { "method": "POST", "path": "/v1/messages" },
      "repeat": 3,
      "response": {
        "status": 200,
        "headers": { "content-type": "text/event-stream" },
        "streaming": true,
        "replay_timing": "realistic",    // or "fast" or { "fixed_delay_ms": 10 }
        "chunks": [
          { "delay_ms": 0, "data": "event: message_start\ndata: {...}\n\n" },
          { "delay_ms": 45, "data": "event: content_block_delta\ndata: {...}\n\n" },
          // ... all chunks inlined
        ],
        "metadata": {
          "provider": "anthropic",
          "model": "claude-sonnet-4-20250514",
          "input_tokens": 12,
          "output_tokens": 85,
          "estimated_cost_usd": 0.00123
        }
      }
    },
    {
      "match": { "method": "POST", "path": "/v1/messages" },
      "repeat": 2,
      "fault": {
        "type": "status",
        "delay_ms": 2000,
        "status": 429,
        "headers": { "retry-after": "30" },
        "body": {
          "type": "error",
          "error": { "type": "rate_limit_error", "message": "Rate limit exceeded" }
        }
      }
    },
    {
      "match": { "method": "POST", "path": "/v1/messages" },
      "repeat": null,    // indefinitely
      "response": {
        "status": 200,
        "headers": { "content-type": "text/event-stream" },
        "streaming": true,
        "replay_timing": "fast",
        "chunks": [ /* ... inlined ... */ ]
      }
    }
  ],
  "unmatched": "passthrough"
}
```

### 6.3 Compilation

`replayr compile` takes an authored scenario + its referenced cassettes and produces a compiled scenario:

```bash
# Compile: resolve references, inline responses, tree-shake unused interactions
replayr compile --scenario ./scenarios/rate-limit.authored.json \
                --cassettes ./cassettes/ \
                --output ./fixtures/rate-limit.scenario.json

# Or compile all scenarios in a directory
replayr compile --dir ./scenarios/ --cassettes ./cassettes/ --output ./fixtures/
```

The UI can also compile on export / when creating a fixture pack.

### 6.4 Built-in Scenario Library

Replayr ships with a library of ready-to-use compiled scenarios. These are **synthetic** (not recorded from real APIs) but crafted to be realistic and provider-accurate.

**General HTTP patterns (any API):**
- `http/rate-limit-429` — 429 after N requests with Retry-After header
- `http/server-error-503` — Intermittent 503 with varying frequency
- `http/timeout` — Connection hangs (configurable duration)
- `http/slow-response` — Valid response with high latency
- `http/connection-reset` — TCP reset mid-response
- `http/auth-expired-401` — 401 after N successful requests

**Anthropic-specific:**
- `anthropic/chat-success` — Realistic streaming chat completion
- `anthropic/chat-success-nonstreaming` — Non-streaming equivalent
- `anthropic/rate-limit-cycle` — Success → 429 → retry-after → success
- `anthropic/overloaded-529` — Anthropic's 529 overloaded error
- `anthropic/streaming-disconnect` — SSE stream cuts off mid-response
- `anthropic/tool-use-flow` — Multi-step tool use conversation
- `anthropic/max-tokens-reached` — Response truncated at max_tokens
- `anthropic/invalid-api-key` — 401 authentication error
- `anthropic/model-not-found` — 404 for deprecated model

**OpenAI-specific:**
- `openai/chat-success` — Streaming chat completion
- `openai/rate-limit-cycle` — With OpenAI-specific error shape
- `openai/content-filter` — Response blocked by content filter
- `openai/function-calling` — Tool/function call flow
- `openai/context-length-exceeded` — 400 error for too-long context

**Usage:**
```bash
# Use a built-in scenario directly
replayr replay --scenario builtin:anthropic/rate-limit-cycle --port 9090

# List all built-in scenarios
replayr library list

# Copy a built-in scenario to customize it
replayr library copy anthropic/rate-limit-cycle ./my-custom-rate-limit.json
# Edit the JSON, then use it
replayr replay --scenario ./my-custom-rate-limit.json --port 9090
```

### 6.5 Built-in Fault Types

| Fault | Description |
|---|---|
| `status` | Return a specific HTTP status code with custom body |
| `delay` | Add latency before responding (fixed or random range) |
| `timeout` | Never respond (connection hangs) |
| `disconnect` | Close connection mid-response (partial streaming) |
| `corrupt` | Return malformed JSON or truncated SSE stream |
| `slow_stream` | Replay SSE chunks with exaggerated delays between them |

### 6.6 Parameterization (future)

Compiled scenarios could support lightweight parameterization for customization without editing JSON:

```bash
# Override the number of successes before rate limiting
replayr replay --scenario builtin:anthropic/rate-limit-cycle \
               --param successes=5 \
               --param retry_after_seconds=60
```

---

## 7. Admin API

Replayr exposes a REST API for control and configuration, used by both the CLI and the UI.

```
# Health
GET    /api/v1/health
GET    /api/v1/ready

# Ring buffer (live traffic)
GET    /api/v1/requests                 List recent interactions from ring buffer
DELETE /api/v1/requests                 Clear the ring buffer
GET    /api/v1/requests/:id             Get a specific interaction (full detail)
WS     /api/v1/ws                       Real-time interaction stream (WebSocket)

# Recording (persist traffic to disk)
GET    /api/v1/record                   Get recording state (on/off, file path, count)
PUT    /api/v1/record                   Toggle recording: { "enabled": true, "output": "./session.json" }
POST   /api/v1/requests/save            Save selected interactions: { "path": "...", "ids": [...] }
POST   /api/v1/requests/save-all        Save entire ring buffer to file

# Mode control
GET    /api/v1/mode
PUT    /api/v1/mode                    { "mode": "proxy", "upstream": "https://api.anthropic.com" }

# Scenarios (when serving)
GET    /api/v1/scenario                 Get active scenario status (current step, counts)
POST   /api/v1/scenario/reset           Reset scenario to step 0

# Cassettes (Milestone 3+)
GET    /api/v1/cassettes
POST   /api/v1/cassettes               (create new / import)
GET    /api/v1/cassettes/:id
DELETE /api/v1/cassettes/:id
GET    /api/v1/cassettes/:id/export?format=har|vcr|hoverfly

# Scenarios management (Milestone 3+)
GET    /api/v1/scenarios
POST   /api/v1/scenarios
GET    /api/v1/scenarios/:id
PUT    /api/v1/scenarios/:id
DELETE /api/v1/scenarios/:id

# Fixture Packs (Milestone 4+)
POST   /api/v1/packs                    Bundle scenarios into downloadable pack
GET    /api/v1/packs/:id                Download (supports ETag/If-None-Match)
```

The proxy itself listens on a separate port from admin (e.g., `:9090` for proxy, `:9091` for admin + inspector UI + WebSocket). The inspector UI is served from the admin port when `--ui` is passed.

---

## 8. CLI

Two primary subcommands: `proxy` (forward traffic with visibility) and `serve` (respond from scenarios). Both support `--ui`, `--log`, and `--record`.

```bash
# ══════════════════════════════════════════════════
# MILESTONE 1 — Proxy with Visibility & Recording
# ══════════════════════════════════════════════════

# Simplest: transparent proxy with CLI logging
replayr proxy --upstream https://api.anthropic.com --port 9090 --log summary

# With inspector UI (watch traffic in browser, save interesting requests)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --ui --admin-port 9091

# With auto-recording (saves everything to disk automatically)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --record --output ./session.json --log summary

# Your app just changes one env var:
# ANTHROPIC_BASE_URL=http://localhost:9090 node my-app.js

# Save specific requests via admin API (after the fact):
# curl -X POST http://localhost:9091/api/v1/requests/save \
#      -d '{"path": "./captured.json", "ids": ["req_1", "req_3"]}'

# Toggle recording on/off via admin API:
# curl -X PUT http://localhost:9091/api/v1/record \
#      -d '{"enabled": true, "output": "./auto.json"}'

# ══════════════════════════════════════════════════
# MILESTONE 2 — Scenario-Based Testing
# ══════════════════════════════════════════════════

# Serve a hand-written scenario (no recording needed)
replayr serve --scenario ./fixtures/rate-limit-test.json --port 9090

# Use a built-in scenario (zero setup, zero files)
replayr serve --scenario builtin:anthropic/rate-limit-cycle --port 9090

# Serve with logging and inspector (see what your app sends + what Replayr returns)
replayr serve --scenario ./fixtures/test.json --port 9090 --log summary --ui

# Browse the built-in scenario library
replayr library list
replayr library list --provider anthropic

# Copy a built-in to customize
replayr library copy anthropic/rate-limit-cycle ./my-rate-limit.json
# Edit the JSON, then serve it:
replayr serve --scenario ./my-rate-limit.json --port 9090

# In your test suite:
# ANTHROPIC_BASE_URL=http://localhost:9090 pytest tests/test_retry.py

# ══════════════════════════════════════════════════
# MILESTONE 3 — Cassette Management & Compilation
# ══════════════════════════════════════════════════

# Inspect a recording
replayr cassette inspect ./session.json

# Compile recording into a self-contained scenario (interactive)
replayr compile --cassette ./session.json --output ./fixtures/my-test.json

# Merge multiple recording sessions
replayr cassette merge ./session-1.json ./session-2.json --output ./combined.json

# Spy mode: serve scenario, record unmatched requests
replayr serve --scenario ./fixtures/my-test.json \
              --upstream https://api.anthropic.com \
              --spy --record-unmatched ./new-captures.json

# ══════════════════════════════════════════════════
# MILESTONE 4 — Export/Import & Distribution
# ══════════════════════════════════════════════════

# Export to HAR
replayr export --cassette ./session.json --format har --output ./out.har

# Import from HAR
replayr import --file ./recording.har --output ./imported.json

# Download fixture pack from hosted Replayr (ETag caching)
replayr pull --server https://replayr.example.com \
             --pack my-test-suite --output ./fixtures/
```

---

## 9. UI — Two Tiers

Replayr has two distinct UIs, built at different milestones for different purposes.

### 9.1 Embedded Request Inspector (Milestone 1)

A lightweight, single-page HTML/JS application **embedded directly in the Rust binary**. No npm, no build step, no separate process. Served on the admin port when `--ui` is passed.

**Purpose:** Watch traffic flowing through the proxy in real time. A debugging and visibility tool.

**Implementation:**
- Static HTML/CSS/JS, embedded via `rust-embed` or `include_str!()`
- WebSocket connection to the admin API for real-time request feed
- Vanilla JS or a tiny framework (Preact/Alpine.js) — whatever compiles to a small bundle
- Total embedded size target: < 100KB gzipped

**Features:**
- Real-time request list (newest first)
- Click to expand: full request headers + body, response headers + body (syntax-highlighted JSON)
- For streaming responses: chunk-by-chunk view with inter-chunk timing visualization
- Filter bar with CEL expressions (e.g. `response.status >= 400 && metadata.provider == "anthropic"`)
- **Mark/star** — mark interesting interactions for batch operations
- **Clear** — flush the ring buffer
- **Save selected / marked / all** — export to cassette file
- **Copy as cURL** — export any interaction as a curl command
- **Replay** — re-send any request to upstream (one-click retry)
- **Record toggle** — auto-recording on/off with visual indicator (file path, count)
- **Intercept / Breakpoints** — set a pattern to pause matching requests before forwarding:
  - Intercepted requests appear highlighted with "Edit / Release / Drop" controls
  - Inline editor for request headers and body before releasing
  - Useful for debugging: "pause, inspect what my app sends, tweak, then forward to Anthropic"
- Status bar: request count, recording state, intercept pattern, active scenario step (in serve mode)
- Dark mode (developer tool aesthetic)

**Not in scope for this UI:**
- Scenario editing or creation
- Cassette management
- Export/import
- Authentication
- Any persistence beyond the current session

### 9.2 Full Scenario Builder UI — TanStack Start (Milestone 6)

The full-featured web application for visual scenario authoring, library browsing, and cassette management. This is a **separate application** that talks to the Admin API.

**Purpose:** Build, manage, and distribute test scenarios visually. For the hosted Cloudflare version, this is the primary interface.

**Tech Stack:**
- **Framework:** TanStack Start (SSR, file-based routing)
- **Styling:** Tailwind CSS
- **State:** TanStack Query for server state
- **Real-time:** WebSocket for live request log
- **Auth (optional):** Clerk — only for hosted multi-tenant version
- **Deployment:** Cloudflare Pages (hosted) or `replayr serve --full-ui` (local, bundled)

**Key Screens:**
- **Dashboard:** Mode indicator, live request log, stats
- **Scenario Library:** Browse built-in and custom scenarios, customize, download
- **Scenario Builder:** Drag-and-drop step editor, fault injection, match rules, live preview
- **Cassette Browser:** Review recordings, select interactions for scenario building
- **Interaction Inspector:** Full request/response viewer, streaming timeline (upgraded from embedded version)
- **Fixture Pack Manager:** Bundle scenarios, generate download URLs, CLI command generator
- **Export:** HAR, VCR, Hoverfly, cURL

---

## 10. Deployment Matrix

| Target | Runtime | Proxy Mode | Storage | Embedded Inspector | Full UI | Admin API |
|---|---|---|---|---|---|---|
| **CLI** | Native binary | Reverse + Forward (later) | Filesystem + SQLite | ✅ (`--ui` flag) | ❌ | ✅ |
| **Docker** | Native binary | Reverse + Forward (later) | Filesystem + SQLite (volume) | ✅ | Optional sidecar | ✅ |
| **Cloudflare Worker** | WASM | Reverse only | R2 + D1 | ✅ | Cloudflare Pages | ✅ |
| **Embedded (later)** | Rust crate | In-process | In-memory | ❌ | ❌ | Programmatic API |

---

## 11. Fixture Pack Distribution (CI/CD Story)

### 11.1 The Primary Unit: Compiled Scenarios

The compiled scenario is the atomic unit of testing. It's a single file, self-contained, with everything inlined. For many users, this is all they need:

```bash
# Simplest possible usage — no recording, no setup
replayr replay --scenario builtin:anthropic/rate-limit-cycle --port 9090 &
ANTHROPIC_BASE_URL=http://localhost:9090 pytest tests/test_retry.py
```

Or with a custom compiled scenario committed to the repo:

```bash
replayr replay --scenario ./fixtures/my-scenario.json --port 9090
```

### 11.2 Fixture Packs (Multiple Scenarios)

For projects with multiple test scenarios, a fixture pack bundles them:

```bash
# Download a pack from hosted Replayr
replayr pull --server https://replayr.example.com \
             --pack my-llm-tests \
             --output ./fixtures/

# Run all scenarios from a directory (each on a separate port, or sequentially)
replayr replay --dir ./fixtures/ --port 9090
```

### 11.3 Three Distribution Strategies

**Strategy A: Commit to repo (small/medium fixtures)**
```
fixtures/
├── anthropic-happy-path.scenario.json      (compiled, self-contained)
├── anthropic-rate-limit.scenario.json
└── openai-timeout.scenario.json
```
Simple, versioned with your code, no external dependencies. Works for most projects.

**Strategy B: Pull from hosted Replayr (large fixtures)**
```bash
# In CI pipeline
replayr pull --server https://replayr.example.com --pack my-suite --output ./fixtures/
# ETag caching: only re-downloads if changed
```
For large recording sets or shared team fixtures. Supports ETag-based caching.

**Strategy C: Use built-in library (zero setup)**
```bash
replayr replay --scenario builtin:anthropic/rate-limit-cycle --port 9090
```
No files, no downloads, no recording. For generic error-handling tests.

### 11.4 Caching Strategy

- Fixture packs have an ETag based on content hash
- `replayr pull` stores the ETag and sends `If-None-Match` on subsequent pulls
- 304 Not Modified → skip download
- Hosted version can also serve via Cloudflare CDN with `Cache-Control` headers

---

## 12. LLM-Specific Features

### 12.1 Provider Detection

Replayr auto-detects the LLM provider from the request URL and headers:

| Provider | Detection | Streaming format |
|---|---|---|
| Anthropic | `/v1/messages`, `x-api-key` header | SSE (`text/event-stream`) |
| OpenAI | `/v1/chat/completions`, `Authorization: Bearer` | SSE (`text/event-stream`) |
| Google (Vertex/Gemini) | `/v1/models/*/generateContent` | SSE or JSON |
| Azure OpenAI | `openai.azure.com` host | SSE |
| Generic | Everything else | Detected from `Content-Type` |

### 12.2 Streaming Recording

For SSE responses, Replayr records:
- Each SSE chunk as a separate entry
- The delay (ms) between consecutive chunks
- Total time-to-first-chunk and time-to-last-chunk

During replay, Replayr can:
- **Realistic replay:** preserve original chunk timing
- **Fast replay:** dump all chunks immediately (for fast tests)
- **Custom timing:** override delays (e.g., simulate slow network)

### 12.3 Token & Cost Tracking

During recording, Replayr extracts token counts from response metadata:
- Anthropic: `usage.input_tokens`, `usage.output_tokens` from `message_stop` event
- OpenAI: `usage.prompt_tokens`, `usage.completion_tokens` from response

Cost is estimated based on a configurable pricing table (model → price per token).

### 12.4 Multi-Turn Awareness (Phase 2+)

For conversation-aware matching:
- Replayr tracks a "conversation" as a sequence of interactions to the same endpoint within a configurable time window
- Request N is matched considering the prior N-1 interactions in the conversation
- This enables testing multi-turn chat flows where each request includes the full conversation history

---

## 13. Rust Crate Architecture

```
replayr/
├── crates/
│   ├── replayr-core/        # Platform-agnostic core logic
│   │   ├── proxy/           # Request forwarding, interception
│   │   ├── matcher/         # Request matching engine
│   │   ├── scenario/        # Scenario execution engine
│   │   ├── compiler/        # Cassette → compiled scenario (Milestone 3)
│   │   ├── cassette/        # Cassette format, serialization
│   │   ├── streaming/       # SSE parsing, chunk recording
│   │   ├── redaction/       # Sensitive data redaction
│   │   ├── provider/        # LLM provider detection & metadata extraction
│   │   └── storage/         # Storage trait + in-memory impl
│   │
│   ├── replayr-library/     # Built-in scenario templates (embedded at compile time)
│   │   ├── scenarios/
│   │   │   ├── http/        # Generic HTTP patterns
│   │   │   ├── anthropic/   # Anthropic-specific
│   │   │   └── openai/      # OpenAI-specific
│   │   └── lib.rs
│   │
│   ├── replayr-inspector/   # Embedded request inspector UI (Milestone 1)
│   │   ├── static/          # HTML/CSS/JS source
│   │   │   ├── index.html
│   │   │   ├── app.js       # Vanilla JS or Preact, < 100KB gzipped
│   │   │   └── style.css
│   │   └── lib.rs           # rust-embed, serves static files + WebSocket
│   │
│   ├── replayr-storage-fs/  # Filesystem + SQLite storage
│   ├── replayr-storage-cf/  # Cloudflare R2 + D1 storage (Milestone 5)
│   │
│   ├── replayr-admin/       # Admin REST API + WebSocket (axum)
│   │
│   ├── replayr-cli/         # CLI binary (clap)
│   │
│   ├── replayr-worker/      # Cloudflare Worker entry point (Milestone 5)
│   │
│   └── replayr-export/      # HAR, VCR, Hoverfly export/import (Milestone 4)
│
├── ui/                      # Full TanStack Start UI (Milestone 6)
│   ├── app/
│   │   ├── routes/
│   │   │   ├── index.tsx
│   │   │   ├── cassettes/
│   │   │   ├── scenarios/
│   │   │   ├── library/
│   │   │   ├── interactions/
│   │   │   └── packs/
│   │   └── components/
│   │       ├── ScenarioBuilder/
│   │       ├── InteractionViewer/
│   │       ├── RequestLog/
│   │       ├── LibraryBrowser/
│   │       └── ChunkTimeline/
│   └── package.json
│
├── Cargo.toml               # Workspace
├── Dockerfile
├── wrangler.toml
└── README.md
```

---

## 14. Milestones

### Milestone 1 — Proxy with Visibility 🎯
**Goal:** A reverse proxy you can point your app at, with request logging, a lightweight request inspector UI, and the ability to save/record traffic on demand.

**Why this first:** Immediately useful as a debugging tool. Forces you to build the core proxy infrastructure (request interception, upstream forwarding, SSE passthrough) which everything else builds on. Saving traffic is recording — but framed as a natural extension of visibility rather than a separate mode.

**Core insight:** Traffic is always captured in an in-memory ring buffer (for the inspector and CLI logger). "Recording" is just persisting that buffer to disk — either retroactively (save what you see) or continuously (auto-record toggle). There's no separate "record mode."

**Features:**

*Core proxy:*
- [ ] Reverse proxy: forward requests to configurable upstream, return responses
- [ ] SSE streaming passthrough (transparent, no buffering)
- [ ] In-memory ring buffer (configurable size, e.g., last 1000 interactions) — always populated, feeds CLI logger, inspector UI, and admin API
- [ ] LLM provider detection (Anthropic, OpenAI) for richer output (model, token counts)
- [ ] Automatic credential redaction in saved/displayed data (`x-api-key`, `Authorization`)

*CLI logging & filtering (CEL — Common Expression Language):*
- [ ] Filter expressions using [CEL](https://cel.dev/) via the `cel-rust` crate
  - `--filter 'request.method == "POST" && request.path.startsWith("/v1/messages")'`
  - `--filter 'response.status >= 400'`
  - `--filter 'request.body.model == "claude-sonnet-4-20250514"'`
  - `--filter 'metadata.provider == "anthropic"'`
  - Filters apply to CLI logging, inspector display, admin API queries, and intercept patterns
- [ ] CEL context exposes: `request` (method, path, headers, body), `response` (status, headers, body), `metadata` (provider, model, tokens, latency)
- [ ] Configurable log verbosity: `--log none|summary|headers|full`
  - `summary`: `POST /v1/messages → 200 (1.2s, 85 tokens, claude-sonnet)`
- [ ] CEL is also used for scenario matchers (Milestone 2) and intercept patterns — one expression language everywhere

*Request/response modification (sed-like):*
- [ ] `--modify-header "name:value"` — set/overwrite a request header
- [ ] `--delete-header "name"` — remove a header
- [ ] `--modify-body "/regex/replacement/"` — sed-style regex replacement in request/response bodies
- [ ] Separator is the first character (like sed): `--modify-body "|old-model|new-model|"`
- [ ] Simple and intentionally limited — for structural JSON transforms, use scenario-level transforms (later milestone)

*Request interception / breakpoints (mitmproxy-inspired):*
- [ ] `--intercept 'CEL expression'` — pause matching requests before forwarding to upstream
  - Example: `--intercept 'request.method == "POST"'`
  - Example: `--intercept 'request.body.model.startsWith("claude")'`
- [ ] Intercepted requests held in a queue, visible in inspector UI and admin API
- [ ] Actions on intercepted requests: inspect, edit (headers/body), release, drop
- [ ] Enables interactive debugging: "pause this request, tweak the prompt, then send it"

*Recording & saving:*
- [ ] `--record --output ./session.json` — auto-persist all traffic to disk
- [ ] Save on demand from inspector UI or admin API (retroactive — save what's already in the buffer)
- [ ] Save selected / save marked / save all

*Admin API (on separate port):*
- [ ] `GET /api/v1/health`
- [ ] `GET /api/v1/requests` — ring buffer contents (supports `?filter=` query param)
- [ ] `GET /api/v1/requests/:id` — full interaction detail
- [ ] `DELETE /api/v1/requests` — clear the ring buffer
- [ ] `POST /api/v1/requests/save` — save interactions to cassette file `{ path, ids? }`
- [ ] `POST /api/v1/requests/:id/replay` — re-send a request to upstream
- [ ] `POST /api/v1/requests/:id/curl` — get curl command for a request
- [ ] `PUT /api/v1/record` — toggle auto-recording `{ enabled, output }`
- [ ] `PUT /api/v1/intercept` — set/clear intercept pattern `{ pattern? }`
- [ ] `GET /api/v1/intercept/queue` — list intercepted (paused) requests
- [ ] `POST /api/v1/intercept/:id/release` — release with optional edits `{ headers?, body? }`
- [ ] `POST /api/v1/intercept/:id/drop` — drop request, don't forward
- [ ] `WS /api/v1/ws` — real-time interaction stream

*Embedded inspector UI (HTML/JS, `rust-embed`, < 100KB gzipped):*
- [ ] Real-time request list (WebSocket-fed, newest first)
- [ ] Click to expand: syntax-highlighted JSON for request/response
- [ ] Streaming chunk timeline with inter-chunk timing visualization
- [ ] Filter bar (CEL expressions, same as CLI `--filter`)
- [ ] **Mark/star** — mark interesting interactions for batch operations
- [ ] **Clear** — flush the ring buffer
- [ ] **Save selected / marked / all** — export to cassette file
- [ ] **Copy as cURL** — export any interaction as a curl command
- [ ] **Replay** — re-send any request to upstream (one-click retry)
- [ ] **Record toggle** — auto-recording on/off with status indicator
- [ ] **Intercept controls** — set breakpoint pattern, view intercepted queue, edit/release/drop
- [ ] Status bar: request count, recording state, active intercept pattern, active scenario step
- [ ] Dark mode
- [ ] No npm, no build step — `--ui` flag activates it

```bash
# Basic proxy with CLI logging
replayr proxy --upstream https://api.anthropic.com --port 9090 --log summary

# With CEL filtering (only log chat completions)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --log summary --filter 'request.path == "/v1/messages"'

# With on-the-fly modification (swap model without changing app code)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --modify-body "|claude-sonnet-4-20250514|claude-haiku-4-5-20251001|"

# With inspector UI (watch, intercept, replay, save)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --ui --admin-port 9091

# With request interception (pause POST requests for inspection)
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --ui --intercept 'request.method == "POST"'

# With auto-recording
replayr proxy --upstream https://api.anthropic.com --port 9090 \
              --record --output ./session.json

# Filter only errors from a specific model
replayr proxy --upstream https://api.anthropic.com --port 9090 --log summary \
              --filter 'response.status >= 400 && request.body.model.contains("sonnet")'
```

**Exit criteria:** Can proxy Anthropic streaming chat completions, see them in CLI log and inspector with chunk timing, intercept and edit a request before forwarding, replay a previous request, copy as cURL, save/mark/export interactions, and toggle auto-recording.

---

### Milestone 2 — Scenario-Based Testing
**Goal:** Define test scenarios in JSON. Run the proxy to serve scripted responses with fault injection. Includes a small built-in scenario library for zero-setup testing.

**Why this second:** This is the core testing value. Combined with Milestone 1's proxy, you can now test error handling, retry logic, and edge cases without hitting real APIs.

**Features:**
- [ ] Compiled scenario format: self-contained JSON with all responses inlined (no external references)
- [ ] Scenario execution engine:
  - Step sequencing with `repeat` (serve N times, then advance)
  - Request matching per step: method + path (exact) for simple cases, CEL for advanced:
    ```jsonc
    // Simple (shorthand)
    "match": { "method": "POST", "path": "/v1/messages" }
    // Advanced (CEL)
    "match": { "cel": "request.method == 'POST' && request.body.model.startsWith('claude')" }
    ```
  - `unmatched` behavior: error (default) or passthrough to upstream
- [ ] Response types per step:
  - Static response (status + headers + body)
  - Streaming response (SSE chunks with timing: realistic, fast, or custom delay)
- [ ] Fault injection per step:
  - `status` — custom HTTP error with body
  - `delay` — add latency before responding (fixed ms or random range)
  - `timeout` — never respond (hang)
  - `disconnect` — close connection mid-response
  - `slow_stream` — SSE chunks with exaggerated inter-chunk delays
- [ ] Built-in scenario library (3-5 templates embedded in binary):
  - `anthropic/rate-limit-cycle` — 3 successes → 429 → recovery
  - `anthropic/overloaded-529` — Anthropic-specific overloaded error
  - `openai/rate-limit-cycle` — OpenAI-flavored rate limit
  - `http/timeout` — Configurable timeout
  - `http/server-error-503` — Intermittent 503
- [ ] CLI commands:
  - `replayr serve --scenario ./my-test.scenario.json --port 9090`
  - `replayr serve --scenario builtin:anthropic/rate-limit-cycle --port 9090`
  - `replayr library list` / `replayr library copy <name> <output>`
  - Combine with Milestone 1: `--log`, `--ui`, `--filter` flags work in serve mode too
- [ ] **Quick fault shortcuts** (no scenario file needed, mitmproxy-inspired):
  - `--block "/v1/embeddings|503"` — return status code for matching path pattern
  - `--map-local "/v1/messages|./mock-response.json"` — serve a local file instead of forwarding
  - Composable with scenarios: `replayr serve --scenario ./test.json --block "/v1/embeddings|503"`

```bash
# Serve a hand-written scenario
replayr serve --scenario ./fixtures/rate-limit-test.json --port 9090

# Use a built-in scenario (zero setup)
replayr serve --scenario builtin:anthropic/rate-limit-cycle --port 9090

# With logging and inspector
replayr serve --scenario ./fixtures/test.json --port 9090 --log summary --ui

# Copy a built-in template to customize
replayr library copy anthropic/rate-limit-cycle ./my-rate-limit.json
# Edit the JSON...
replayr serve --scenario ./my-rate-limit.json --port 9090

# In your test suite:
ANTHROPIC_BASE_URL=http://localhost:9090 pytest tests/test_retry.py
```

**Exit criteria:** Can serve a scenario that returns 3 streaming Anthropic responses, then 2 rate limit errors, then recovers. Can use `builtin:anthropic/rate-limit-cycle` with zero files.

---

### Milestone 3 — Cassette Management & Compilation
**Goal:** Turn raw recordings (from Milestone 1) into compiled scenarios. Manage, browse, and transform cassette files.

**Why now:** Milestone 1 already captures and saves traffic. This milestone adds the tooling to go from raw recordings → curated test scenarios.

**Features:**
- [ ] Cassette format v1 (formalize the save format from Milestone 1)
- [ ] Token count extraction from response metadata (Anthropic, OpenAI)
- [ ] Cost estimation (configurable pricing table per model)
- [ ] Configurable redaction patterns (regex, beyond the auto-redaction from M1)
- [ ] `replayr compile` — interactive CLI to convert cassette → compiled scenario:
  - Select which interactions to include (by index, path filter, or provider)
  - Insert faults between interactions (e.g., "after interaction 3, add a 429")
  - Set replay timing (realistic, fast, custom)
  - Output a single self-contained scenario file
- [ ] `replayr cassette inspect` — summarize a cassette (interaction count, providers, models, total tokens, cost)
- [ ] **Response refreshing** (mitmproxy-inspired): when replaying, auto-update `Date`, `x-request-id`, and cookie expiry headers to maintain relative time offsets from recording time
- [ ] `replayr cassette merge` — combine multiple recording sessions
- [ ] Spy mode: serve from scenario, passthrough + record unmatched requests

```bash
# Inspect what you recorded
replayr cassette inspect ./session.json
# → 12 interactions, 3 providers, 847 tokens, ~$0.02

# Compile into a scenario (interactive)
replayr compile --cassette ./session.json --output ./fixtures/my-test.json
# → Select interactions [1,3,7], insert 429 after step 2, set timing to "fast"

# Spy mode: replay scenario, record anything new
replayr serve --scenario ./fixtures/my-test.json \
              --upstream https://api.anthropic.com \
              --spy --record-unmatched ./new-captures.json
```

**Exit criteria:** Can take a 20-interaction recording session, compile 4 interactions into a scenario with a fault injected, and replay it.

---

### Milestone 4 — Export/Import & Distribution
**Goal:** Interop with existing tools. CI-friendly fixture distribution.

- [ ] HAR export/import
- [ ] VCR.py YAML export/import
- [ ] Hoverfly JSON export/import
- [ ] cURL export (per interaction)
- [ ] Fixture Pack bundling (multiple compiled scenarios → archive)
- [ ] `replayr pull` with ETag caching
- [ ] Pack versioning
- [ ] Docker image published to registry

**Exit criteria:** CI pipeline can `replayr pull` a fixture pack and run tests against it with caching.

---

### Milestone 5 — Cloudflare Worker Deployment
**Goal:** Run Replayr on Cloudflare Workers with R2/D1.

- [ ] WASM compilation target
- [ ] R2 storage backend
- [ ] D1 metadata backend
- [ ] wrangler.toml configuration
- [ ] Fixture Pack serving via CDN

**Exit criteria:** Hosted Replayr instance recording and replaying on the edge.

---

### Milestone 6 — Full UI (TanStack Start)
**Goal:** Visual scenario authoring, library browsing, and cassette management.

- [ ] TanStack Start project setup
- [ ] Dashboard with live request log
- [ ] Scenario library browser (browse built-ins, customize, download)
- [ ] Scenario builder (drag-and-drop steps, fault config, match rules)
- [ ] Cassette browser (review recordings, select interactions)
- [ ] Compile & download from UI
- [ ] Fixture Pack manager
- [ ] Export dialog (HAR, VCR, Hoverfly, cURL)

**Exit criteria:** Can build a custom scenario visually and download a compiled fixture.

---

### Milestone 7 — Multi-Tenancy & Auth (Hosted)
- [ ] Clerk integration (optional, feature-flagged)
- [ ] User/team/workspace model
- [ ] API key authentication
- [ ] Usage tracking

### Milestone 8+ — Advanced Features
- [ ] Forward proxy mode with TLS MITM (CLI/Docker)
- [ ] LLM-aware smart matching (fuzzy prompt matching, provider presets)
- [ ] Multi-turn conversation sequence matching
- [ ] Scenario parameterization (`--param successes=5`)
- [ ] OpenAPI spec generation from recorded traffic (à la mitmproxy2swagger)
- [ ] Embeddable Rust crate (in-process API)
- [ ] Language bindings (Python, Node.js)
- [ ] Test framework integrations (pytest plugin, vitest plugin)
- [ ] Community scenario library
- [ ] Plugin/scripting system (WASM plugins or Lua scripting for custom matchers/transformers)
- [ ] WebSocket / GraphQL support

---

## 15. Key Technical Decisions

### 15.1 Decisions Made

| Decision | Choice | Rationale |
|---|---|---|
| Language | Rust | Compiles to native + WASM, performance, safety |
| Proxy interception | Reverse proxy (MVP), forward proxy (later) | Reverse works on all runtimes including Workers |
| Cassette format | Own JSON format | LLM-specific metadata (streaming, tokens) doesn't fit HAR/VCR |
| Metadata storage | SQLite / D1 | Portable, zero-config, works on Cloudflare edge |
| Blob storage | Filesystem / R2 | Cassette bodies can be large |
| UI framework | TanStack Start | SSR, file-based routing, TypeScript, future auth integration |
| Auth | Clerk (optional) | Only for hosted version, won't pollute the core |
| Matching (MVP) | Exact match on method + path + normalized body | Simple, deterministic, covers 80% of cases |
| Expression language | CEL (Common Expression Language) via `cel-rust` | Google-backed standard, Rust crate available, compiles to WASM, extensible. One language for filtering, matching, and intercept patterns |
| Transforms | Sed-like regex replacement | Simple, familiar, CLI-friendly. Structural transforms deferred to later milestones |
| Multi-turn (MVP) | Independent matching | Sequence matching adds complexity; defer to Phase 2+ |

### 15.2 Decisions Pending

| Decision | Options | Notes |
|---|---|---|
| License | MIT, Apache 2.0, BSL | Affects contribution model and commercial hosting story |
| Crate name on crates.io | `replayr` | Check availability |
| Config file format | TOML vs YAML vs JSON | TOML is Rust-idiomatic; JSON is universal |
| Admin API framework | `axum` vs `actix-web` | Both work; axum has better tower/WASM compatibility |
| SQLite library | `rusqlite` vs `libsql` | `libsql` has better Cloudflare/edge story |
| Scenario DSL | JSON only vs. also a compact DSL | JSON is universal but verbose for hand-authoring |

---

## 16. Competitive Positioning

| Feature | Replayr | Hoverfly | WireMock | MockServer | VCR.py |
|---|---|---|---|---|---|
| Language | Rust | Go | Java | Java | Python |
| WASM / Edge | ✅ | ❌ | ❌ | ❌ | ❌ |
| CLI binary | ✅ | ✅ | ❌ (needs JVM) | ❌ (needs JVM) | ❌ (library) |
| Docker | ✅ | ✅ | ✅ | ✅ | ❌ |
| Embeddable | ✅ (crate) | ❌ | ✅ (JUnit) | ✅ (Java API) | ✅ (decorator) |
| SSE streaming | ✅ (first-class) | ❌ | ❌ | ❌ | Partial |
| LLM-aware | ✅ | ❌ | ❌ | ❌ | ❌ |
| Token tracking | ✅ | ❌ | ❌ | ❌ | ❌ |
| Scenario builder UI | ✅ | Cloud only | Cloud only | ❌ | ❌ |
| Fault injection | ✅ | ✅ | ✅ | ✅ | ❌ |
| Streaming faults | ✅ | ❌ | ❌ | ❌ | ❌ |
| Built-in scenario library | ✅ | ❌ | ❌ | ❌ | ❌ |
| Zero-setup testing | ✅ (builtins) | ❌ | ❌ | ❌ | ❌ |
| Self-contained fixtures | ✅ (compiled) | ❌ (refs) | JSON files | Expectations | YAML cassettes |
| General-purpose HTTP | ✅ | ✅ | ✅ | ✅ | ✅ |

---

## 17. Non-Goals (Explicit Exclusions)

To keep scope manageable:

- **Not a full API gateway** — No rate limiting, auth, or routing for production traffic
- **Not an observability tool** — No distributed tracing, metrics dashboards, or alerting
- **Not a load testing tool** — No concurrent virtual users or throughput benchmarking (though recorded cassettes could feed into a load testing tool)
- **Not a contract testing tool** — No OpenAPI validation or consumer-driven contract testing (though OpenAPI export is planned)
- **No WebSocket support in MVP** — HTTP request/response and SSE only initially

---

## 18. Open Questions for Future Exploration

1. **Community scenario library?** Could Replayr host a public registry of user-contributed scenarios (like npm for API test fixtures)? Users could publish and share compiled scenarios for niche APIs, specific error patterns, etc.
2. **Should Replayr support gRPC?** Some LLM providers may move to gRPC. WASM support for gRPC is limited.
3. **Could the compiled scenario format become a standard?** If Replayr gains adoption, the format could be proposed as an open standard for API interaction recording with streaming support.
4. **Plugin system?** Allow users to write custom matchers, transformers, or fault generators. Rust WASM plugins? Lua scripting?
5. **AI-assisted scenario generation?** "Record 10 interactions, then use an LLM to generate edge-case scenarios based on the API's behavior."
6. **Cost budgeting?** Track cumulative LLM spend across recording sessions, alert when approaching a budget.
7. **How far can the built-in library go?** If the library covers most common scenarios well enough, recording becomes a power-user feature rather than the default entry point. This changes the product story significantly — from "record/replay tool" to "API test scenario platform."
8. **Provider-specific response generators?** Instead of static synthetic responses in the library, could Replayr generate realistic responses dynamically? E.g., "generate a realistic Anthropic streaming response for a 200-word answer" — using response shape templates, not actual LLMs.

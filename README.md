# replayr

![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/sagikazarmark/replayr/ci.yaml?style=flat-square)
![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/sagikazarmark/replayr/badge?style=flat-square)

`replayr` is an API record/replay/simulation proxy.

It runs two HTTP servers:

- Proxy server (default `9090`) for pass-through traffic to an upstream API.
- Admin server (default `9091`) for health, request history, replay controls, and optional UI.

## Prerequisites

- Rust + Cargo (for local builds)
- Docker (for containerized runs)

## Local usage

Build:

```bash
cargo build --release
```

Run:

```bash
./target/release/replayr proxy \
  --upstream https://httpbin.org \
  --port 9090 \
  --admin-port 9091 \
  --ui
```

Useful flags:

- `--bind` (default: `127.0.0.1`) bind host for both proxy/admin listeners
- `--record` enable request recording
- `--output ./session.json` output path for recorded session data

## Docker

Build image:

```bash
docker build -t replayr:local .
```

Run container:

```bash
docker run --rm -it \
  -p 9090:9090 \
  -p 9091:9091 \
  -v "$(pwd)/data:/data" \
  replayr:local \
  proxy \
  --upstream https://httpbin.org \
  --bind 0.0.0.0 \
  --port 9090 \
  --admin-port 9091 \
  --ui \
  --record \
  --output /data/session.json
```

## Docker Compose example

An example compose file is available at `docker-compose.example.yml`.

Run it:

```bash
docker compose -f docker-compose.example.yml up --build
```

## Endpoints

- Proxy: `http://localhost:9090`
- Admin health: `http://localhost:9091/api/v1/health`
- Admin UI (when `--ui` is set): `http://localhost:9091/`

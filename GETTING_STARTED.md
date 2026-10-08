# Getting started with innerrag

This guide goes from a blank machine to the Assistant answering questions. It applies to Linux, macOS
and Windows, with Docker or Podman.

innerrag runs in **one Linux container**: the server, the web interface and the models are all in the
image. There is no Rust or Node to install. The network is only needed to build the image.

```
 browser ──► http://localhost:8080 ──► innerrag container (server, UI, models, ./data)
    │
    └─(Assistant page)──► bridge http://127.0.0.1:18765 ──► claude / copilot installed on the machine
```

The **bridge** is a small Python script that runs on the machine, outside the container: a container
cannot start the host's Claude Code or Copilot CLI. Only the Assistant page needs it; everything else
works without it.

## 1. Prerequisites

| What | Why | Check |
|---|---|---|
| Docker (with buildx) **or** Podman | Build and run the image | `docker info` or `podman info` |
| 8 GB of memory for the engine | The Rust build and the models are heavy | Docker Desktop settings, or `podman machine inspect` |
| Python 3 | The Assistant's bridge only | `python3 --version` (Windows: `py --version`) |
| Claude Code and/or GitHub Copilot CLI, signed in | The Assistant's agents | `claude --version`, `copilot --version` |

Per system:

- **Windows**: Docker Desktop in "Linux containers" mode, or Podman Desktop. With Podman, its virtual
  machine must be running: `podman machine start` (the first time: `podman machine init`).
- **macOS**: Docker Desktop, OrbStack, Colima or Podman (`podman machine start`).
- **Linux**: Docker Engine or Podman, no virtual machine.

## 2. Build the image

From the repository root:

```bash
scripts/build.sh                 # Linux, macOS, WSL
```
```powershell
.\scripts\build.ps1              # Windows (PowerShell)
```

The script uses Docker when it is installed, otherwise Podman (`--engine podman` / `-Engine podman`
forces it). It produces `innerrag:latest` for the machine's architecture. The first build takes about
10 minutes; later ones reuse the cache.

### Behind a corporate proxy (Zscaler…)

If the build fails with `SSL certificate problem: self-signed certificate in certificate chain` or
`UnknownIssuer`, the proxy re-signs some sites with a company authority the Linux images do not know.
Put that authority in `certs/`:

- **Windows**: `.\scripts\build.ps1 -TrustWindowsCa "<name>"` exports the valid certificates of the
  Windows store whose subject contains that name (for example `Zscaler`, or the company PKI's name).
- **macOS / Linux**: copy the company root, in PEM format, to `certs/<name>.crt`.

Everything in `certs/` is ignored by git: these certificates are never pushed.

## 3. Start the server

With Docker Compose (or `podman compose` when a compose provider is installed):

```bash
docker compose up -d
```

Without compose, the equivalent command:

```bash
docker run -d --name innerrag -p 127.0.0.1:8080:8080 -v "$PWD/data:/data" --restart unless-stopped innerrag:latest
```
```powershell
podman run -d --name innerrag -p 127.0.0.1:8080:8080 -v "${PWD}\data:/data" --restart unless-stopped localhost/innerrag:latest
```

Check it, then open the interface:

```bash
curl http://127.0.0.1:8080/api/health        # {"status":"ok",...}
```

→ **http://localhost:8080**

The port is published on `127.0.0.1` only: the server has no authentication.

### Where the data lives

Everything is in `./data`, next to the repository (ignored by git):

```
data/
  projects/<id>/   one database per project (innerrag.lbdb, project.json, files/…)
  history/         the call history
```

Removing the container loses nothing, and neither does rebuilding the image.

## 4. Keep a folder in sync (specifications, for example)

A folder mounted under `/watch` can be followed by a project: its files (PDF, Word, PowerPoint,
Markdown, HTML, text) are imported, then synced again every 30 seconds. Additions, changes and
deletions are applied, and only the passages that changed are recomputed.

1. Mount the folder read-only, for example `data/specs`:

   ```powershell
   podman run -d --name innerrag -p 127.0.0.1:8080:8080 `
     -v "${PWD}\data:/data" -v "${PWD}\data\specs:/watch/specs:ro" `
     --restart unless-stopped localhost/innerrag:latest
   ```

   With compose, add `- ./data/specs:/watch/specs:ro` under `volumes:`.

2. Attach the folder to a project: **Projects** page › watched folder › `specs`. Or:

   ```bash
   curl -X PATCH http://127.0.0.1:8080/api/projects/<id> -H "Content-Type: application/json" -d '{"watch_dir":"specs"}'
   ```

Never watch `data/` itself: the server keeps a copy of the original files there
(`data/projects/*/files/`), and they would be imported again.

Renaming a project changes its **title**, shown in the interface. Its **id** stays the one it was
created with: it names the `data/projects/<id>/` folder and the `/mcp/<id>` MCP address.

## 5. The Assistant: start the bridge

The Assistant page talks to Claude Code or Copilot CLI on the machine, through the bridge. Start it
for each working session; nothing installs it to run when the system starts.

```bash
python3 scripts/chat-bridge.py          # macOS, Linux
```
```powershell
py scripts\chat-bridge.py               # Windows
```

- The bridge finds the server by itself (`http://localhost:8080`, else `http://localhost:18080`). If the
  server is published elsewhere: `--innerrag http://localhost:8081`.
- It listens on `127.0.0.1:18765` and only answers pages of the interface.
- It uses the accounts Claude Code and Copilot are already signed in with: no API key.
- By default the agents can only read the knowledge base; `--allow-writes` also lets them ingest.
- Stop it with Ctrl+C, or by closing its window.

To run it on Windows without keeping a window open (it stops when you sign out):

```powershell
Start-Process py -ArgumentList 'scripts\chat-bridge.py' -WindowStyle Hidden
```

If the Assistant page says the bridge does not answer: it is not running, or it was started with a
server address different from the page's (see `--innerrag`).

## 6. Day to day

| Action | Command |
|---|---|
| Stop / start again | `docker stop innerrag` / `docker start innerrag` (or `podman …`, `docker compose stop/start`) |
| Follow the logs | `docker logs -f innerrag` |
| Update after a `git pull` | Rebuild (section 2), then `docker rm -f innerrag` and start again (section 3), or `docker compose up -d --build` |
| Podman on Windows or macOS, after a reboot | `podman machine start`: the container restarts by itself (`--restart unless-stopped`) |

The interface is compiled into the image: a change to `ui/` only shows after a rebuild.

## 7. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `/usr/bin/env: 'bash\r'` under WSL | Repository checked out with Windows line endings before `.gitattributes` existed | `git rm --cached -r . && git reset --hard` |
| `docker is not installed` on Windows | Only Podman is installed | The scripts fall back to Podman; use `podman` in the commands |
| `podman cannot reach its machine` | Podman machine stopped | `podman machine start` |
| `curl: (60)` or `UnknownIssuer` during the build | Proxy that inspects TLS | Section 2, "Behind a corporate proxy" |
| Build killed during `cargo build` | Not enough memory | Give the engine 8 GB |
| `port is already allocated` | Port 8080 taken | `-p 127.0.0.1:8081:8080`, and `--innerrag http://localhost:8081` for the bridge |
| Importing a web address fails behind the proxy | The server's HTTP client only trusts its own authorities | Download the file and upload it, or drop it in a watched folder |
| Assistant: "the bridge does not answer" | Bridge not running | Section 5 |

Further reading: environment variables, REST API and MCP in the [README](README.md).

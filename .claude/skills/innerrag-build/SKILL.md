---
name: innerrag-build
description: Build and start the innerrag Docker image correctly on any OS — Linux, macOS (Intel or Apple Silicon), Windows with Docker Desktop, Podman or WSL — including behind a corporate TLS-inspecting proxy (Zscaler…), the GPU variant, and the usual build failures. Use for "build le projet", "construis l'image", "démarre l'application", "lance innerrag", "le build échoue", "certificate problem", "UnknownIssuer", "bash\r", or setting up a dev environment.
---

# Building innerrag on any OS

innerrag ships as **one Linux image** (Rust server, React UI, ONNX models). Rust and Node are not needed
on the machine: everything compiles inside the build. The only requirement is a container engine that
runs **Linux containers**: Docker (with buildx) or Podman. The network is needed only during the build.

## 1. Pick the command for the machine

| OS | Engine | Command (from the repository root) |
|---|---|---|
| Linux | Docker or Podman | `scripts/build.sh` |
| macOS (Intel / Apple Silicon) | Docker Desktop, OrbStack, Colima or Podman | `scripts/build.sh` |
| Windows | Docker Desktop (Linux containers) or Podman Desktop | `.\scripts\build.ps1` |
| Windows, inside WSL | Docker / Podman reachable from the distro | `scripts/build.sh` |

Both scripts pick Docker when it is installed, otherwise Podman; force one with `--engine podman` /
`-Engine podman`. They build the `cpu` target for the machine's architecture and tag `innerrag:latest`.

Check the engine first, so a failure is not mistaken for a build problem:

- Docker: `docker info --format '{{.OSType}}'` must print `linux` (on Windows, Docker Desktop >
  *Switch to Linux containers*), and `docker buildx version` must answer.
- Podman on macOS / Windows: `podman machine start` (once: `podman machine init`), then `podman info`.
  Give the machine at least 4 CPUs and 8 GB of memory: the Rust link step and the models are heavy.

The first build takes about 10 minutes (more under Podman or emulation); the image is about 1.5 GB.
Later builds reuse the cargo and layer caches.

## 2. Corporate proxy that inspects TLS (Zscaler, Netskope…)

Symptoms: `curl: (60) SSL certificate problem: self-signed certificate in certificate chain` while
downloading the models, or `invalid peer certificate: UnknownIssuer` from a Rust build script. The
proxy re-signs some sites (often CDNs such as `*.cdn.hf.co`, not the main site) with a company root the
Linux images do not trust. Confirm it from inside a container:

```bash
docker run --rm debian:trixie-slim sh -c "apt-get -qq update >/dev/null && apt-get -qq install -y openssl >/dev/null; \
  echo | openssl s_client -connect us.aws.cdn.hf.co:443 -servername us.aws.cdn.hf.co 2>/dev/null | grep 'i:'"
```

An issuer naming the company or the proxy vendor confirms it. Fix: put the company root (PEM, `.crt`)
in `certs/`. The Dockerfile trusts every `certs/*.crt` in the stages that download and in the final image.

- **Windows**: `.\scripts\build.ps1 -TrustWindowsCa "<subject pattern>"` exports the matching, still
  valid authorities of the Windows store into `certs/windows-ca.crt` (find the pattern in the issuer
  printed above, e.g. `Zscaler` or the company PKI name).
- **macOS**: `security find-certificate -a -c "<name>" -p /Library/Keychains/System.keychain > certs/company-ca.crt`.
- **Linux**: copy it from `/usr/local/share/ca-certificates/` or ask the IT team.

**Never commit these certificates**: `certs/` is git-ignored except its README. Changing `certs/`
invalidates the layers after it, so the models are downloaded again.

The running server needs no network, except `ingest_url` (web pages): its HTTP client (ureq + rustls)
trusts only its built-in roots, so behind such a proxy importing a web address can still fail.

## 3. Start and check

```bash
docker compose up -d          # or: podman compose up -d   (needs a compose provider)
# without compose:
docker run -d --name innerrag -p 127.0.0.1:8080:8080 -v "$PWD/data:/data" innerrag:latest
curl -fsS http://127.0.0.1:8080/api/health    # {"status":"ok",...}
```

PowerShell: `-v "${PWD}\data:/data"`. Open http://localhost:8080. Data (one folder per project,
history) lives in `./data`. A folder to keep in sync is mounted under `/watch`
(`-v ./my-docs:/watch/my-docs:ro`) and attached to a project from the Projects page or with
`PATCH /api/projects/{id}` `{"watch_dir":"my-docs"}`. Never watch `./data` itself: the server keeps the
original files in `data/projects/*/files/`, and the watcher would import them again.

The UI is compiled into the image: after changing `ui/`, rebuild and recreate the container
(`docker rm -f innerrag` then `docker run …`, or `docker compose up -d --build`).

## 4. Variants

- **GPU (NVIDIA, Linux or Windows/WSL2)**: `scripts/build.sh --gpu` / `.\scripts\build.ps1 -Gpu`, then
  `docker compose --profile gpu up -d innerrag-gpu`. Needs the NVIDIA Container Toolkit; not possible on macOS.
- **Another architecture**: `--platform amd64|arm64` (emulated, slow). `--platform all --push --tag
  registry/innerrag:x` builds a multi-arch image; Docker buildx only (with Podman, build each one).
- **Offline transfer**: `--save innerrag.tar` / `-Save innerrag.tar`, then `docker load -i innerrag.tar`.
- **UI development**: `cd ui && npm install && npm run dev` (port 5173, proxied to a server on :18080).

## 5. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `/usr/bin/env: 'bash\r': No such file or directory` (WSL, Linux) | Checked out on Windows with CRLF before `.gitattributes` existed | `git rm --cached -r . && git reset --hard`, or `dos2unix scripts/*.sh` |
| `docker is not installed or not in PATH` on Windows | Podman only | The scripts fall back to Podman; for compose files use `podman compose` |
| `Docker runs Windows containers` | Docker Desktop in Windows-containers mode | Switch to Linux containers |
| `podman cannot reach its machine` | Machine stopped | `podman machine start` |
| `curl: (60)` / `UnknownIssuer` | TLS-inspecting proxy | Section 2 |
| `failed to run custom build command for ort-sys` | A build script tried to download ONNX Runtime | Already avoided by `ORT_LIB_LOCATION` in the Dockerfile; keep it when editing the builder stage |
| Killed / out of memory during `cargo build` | Engine VM too small | Give Docker Desktop / the Podman machine 8 GB |
| `port is already allocated` | Port 8080 taken | `-p 127.0.0.1:8081:8080` |
| Container restarts, `refuses to open` a project | Project indexed with another embedding model | Rebuild with the same `EMBED_REPO`/`EMBED_FILE`, or re-index |

When a step fails, read the build log from the first `error`, not the last line: Podman and buildx
print the failing `RUN` command just above it.

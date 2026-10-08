#!/usr/bin/env bash
# Build the innerrag Docker image (Linux image; run from Linux or macOS).
#
#   scripts/build.sh                         # image for this machine's architecture
#   scripts/build.sh --platform amd64        # linux/amd64 (servers, most PCs)
#   scripts/build.sh --platform arm64        # linux/arm64 (Apple Silicon, Graviton)
#   scripts/build.sh --platform all --push --tag registry.example.com/innerrag:0.1.0
#   scripts/build.sh --save innerrag.tar     # export the image to a file (offline transfer)
#   scripts/build.sh --gpu                   # NVIDIA GPU image innerrag:cuda (linux/amd64)
#   scripts/build.sh --engine podman         # force Podman (default: docker, else podman)
#
# Options:
#   --tag NAME          image name (default innerrag:latest)
#   --platform P        native | amd64 | arm64 | all (all needs --push: multi-arch images live in a registry)
#   --push              push to the registry instead of loading locally
#   --gpu               CUDA image (target runtime-cuda, linux/amd64, default tag innerrag:cuda)
#   --save FILE         after building, docker save the image to FILE
#   --no-cache          rebuild every layer
#   --ner-file F        GLiNER ONNX file (default onnx/model_fp16.onnx)
#   --embed-file F      e5 ONNX file (default onnx/model_quantized.onnx)
#   --engine E          auto | docker | podman
#
# Behind a TLS-inspecting proxy (Zscaler...), put the company root CA in certs/ (see certs/README.md).
set -euo pipefail

cd "$(dirname "$0")/.."

TAG=""
PLATFORM="native"
GPU=0
PUSH=0
SAVE=""
EXTRA=()
ENGINE="auto"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="$2"; shift 2 ;;
    --platform) PLATFORM="$2"; shift 2 ;;
    --push) PUSH=1; shift ;;
    --gpu) GPU=1; shift ;;
    --save) SAVE="$2"; shift 2 ;;
    --no-cache) EXTRA+=(--no-cache); shift ;;
    --ner-file) EXTRA+=(--build-arg "NER_FILE=$2"); shift 2 ;;
    --embed-file) EXTRA+=(--build-arg "EMBED_FILE=$2"); shift 2 ;;
    --engine) ENGINE="$2"; shift 2 ;;
    -h|--help) sed -n '2,23p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

if [[ $GPU -eq 1 ]]; then
  [[ "$PLATFORM" == "native" || "$PLATFORM" == "amd64" ]] || { echo "--gpu builds linux/amd64 only" >&2; exit 2; }
  PLATFORM="amd64"
  TAG="${TAG:-innerrag:cuda}"
  EXTRA+=(--target runtime-cuda)
  if [[ "$(uname -m)" != "x86_64" ]]; then
    echo "note: building amd64 on $(uname -m) runs under emulation and is slow (expect 30+ min)" >&2
  fi
else
  TAG="${TAG:-innerrag:latest}"
  EXTRA+=(--target cpu)
fi

if [[ "$ENGINE" == "auto" ]]; then
  if command -v docker >/dev/null; then ENGINE=docker
  elif command -v podman >/dev/null; then ENGINE=podman
  else echo "neither docker nor podman is installed or in PATH" >&2; exit 1; fi
fi
case "$ENGINE" in
  docker)
    command -v docker >/dev/null || { echo "docker is not installed or not in PATH" >&2; exit 1; }
    docker buildx version >/dev/null 2>&1 || { echo "docker buildx is required (Docker 23+ / Docker Desktop)" >&2; exit 1; } ;;
  podman)
    command -v podman >/dev/null || { echo "podman is not installed or not in PATH" >&2; exit 1; }
    podman info >/dev/null 2>&1 || { echo "podman cannot reach its engine (macOS/Windows: podman machine start)" >&2; exit 1; } ;;
  *) echo "--engine must be auto, docker or podman" >&2; exit 2 ;;
esac

case "$PLATFORM" in
  native) PLATFORMS="" ;;
  amd64) PLATFORMS="linux/amd64" ;;
  arm64) PLATFORMS="linux/arm64" ;;
  all) PLATFORMS="linux/amd64,linux/arm64" ;;
  *) echo "--platform must be native, amd64, arm64 or all" >&2; exit 2 ;;
esac

if [[ "$PLATFORM" == "all" && $PUSH -eq 0 ]]; then
  echo "--platform all builds a multi-architecture image, which must be pushed: add --push and a registry --tag" >&2
  exit 2
fi
if [[ "$PLATFORM" == "all" && "$ENGINE" == "podman" ]]; then
  echo "--platform all needs docker buildx; with podman, build and push each architecture (--platform amd64 / arm64)" >&2
  exit 2
fi

if [[ "$ENGINE" == "docker" ]]; then ARGS=(buildx build); else ARGS=(build); fi
ARGS+=(--tag "$TAG" ${EXTRA[@]+"${EXTRA[@]}"})
[[ -n "$PLATFORMS" ]] && ARGS+=(--platform "$PLATFORMS")
if [[ "$ENGINE" == "docker" ]]; then
  if [[ $PUSH -eq 1 ]]; then ARGS+=(--push); else ARGS+=(--load); fi
fi
if [[ "$PLATFORM" == "all" ]]; then
  # The default "docker" driver cannot build several platforms at once.
  docker buildx inspect innerrag-builder >/dev/null 2>&1 || docker buildx create --name innerrag-builder --driver docker-container >/dev/null
  ARGS+=(--builder innerrag-builder)
fi

echo "==> $ENGINE ${ARGS[*]} ."
start=$(date +%s)
"$ENGINE" "${ARGS[@]}" .
echo "==> built $TAG in $(( $(date +%s) - start )) s"
# Podman builds into its local store; pushing is a separate step.
if [[ $PUSH -eq 1 && "$ENGINE" == "podman" ]]; then podman push "$TAG"; fi

if [[ $PUSH -eq 0 ]]; then
  "$ENGINE" image ls "$TAG" --format '    {{.Repository}}:{{.Tag}}  {{.Size}}'
fi
if [[ -n "$SAVE" ]]; then
  echo "==> saving to $SAVE"
  "$ENGINE" save "$TAG" -o "$SAVE"
  echo "    load it elsewhere with: $ENGINE load -i $SAVE"
fi
echo "==> run it: $ENGINE run -d -p 127.0.0.1:8080:8080 -v \"\$PWD/data:/data\" $TAG   (or: $ENGINE compose up -d)"

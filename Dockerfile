# syntax=docker/dockerfile:1.7
#
# innerrag: Graph RAG server (Rust + LadybugDB) with embedded ONNX models and React UI.
# Network is only needed at build time; the resulting image runs fully offline.

ARG EMBED_REPO=Xenova/multilingual-e5-small
ARG EMBED_FILE=onnx/model_quantized.onnx
ARG NER_REPO=onnx-community/gliner_multi-v2.1
ARG NER_FILE=onnx/model_fp16.onnx
# ONNX Runtime version expected by ort 2.0.0-rc.9
ARG ORT_VERSION=1.20.1
# CUDA libraries for the GPU image (ONNX Runtime 1.20 GPU: CUDA 12.x + cuDNN 9)
ARG CUDA_IMAGE=nvidia/cuda:12.6.3-cudnn-runtime-ubuntu24.04

# ---- React UI ---------------------------------------------------------------
FROM node:22-trixie-slim AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

# ---- Models and ONNX Runtime ------------------------------------------------
FROM debian:trixie-slim AS models
ARG EMBED_REPO EMBED_FILE NER_REPO NER_FILE ORT_VERSION TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /models
RUN set -eux; \
    hf() { curl -fsSL --retry 3 -o "$3" "https://huggingface.co/$1/resolve/main/$2"; }; \
    mkdir -p embed ner; \
    hf "$EMBED_REPO" "$EMBED_FILE" embed/model.onnx; \
    hf "$EMBED_REPO" tokenizer.json embed/tokenizer.json; \
    hf "$NER_REPO" "$NER_FILE" ner/model.onnx; \
    hf "$NER_REPO" tokenizer.json ner/tokenizer.json; \
    hf "$NER_REPO" gliner_config.json ner/gliner_config.json
RUN set -eux; \
    case "$TARGETARCH" in amd64) arch=x64 ;; arm64) arch=aarch64 ;; *) echo "unsupported $TARGETARCH"; exit 1 ;; esac; \
    curl -fsSL --retry 3 "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-${arch}-${ORT_VERSION}.tgz" | tar xz -C /tmp; \
    mkdir -p /ort; \
    cp -L /tmp/onnxruntime-linux-${arch}-${ORT_VERSION}/lib/libonnxruntime.so.${ORT_VERSION} /ort/libonnxruntime.so

# ONNX Runtime GPU build (CUDA execution provider), linux/amd64 only.
FROM debian:trixie-slim AS ort-gpu
ARG ORT_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    curl -fsSL --retry 3 "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-gpu-${ORT_VERSION}.tgz" | tar xz -C /tmp; \
    mkdir -p /ort; \
    lib=/tmp/onnxruntime-linux-x64-gpu-${ORT_VERSION}/lib; \
    cp -L $lib/libonnxruntime.so.${ORT_VERSION} /ort/libonnxruntime.so; \
    cp -L $lib/libonnxruntime_providers_shared.so $lib/libonnxruntime_providers_cuda.so /ort/

# ---- Rust server ------------------------------------------------------------
# trixie: LadybugDB headers need GCC >= 13 (<format>).
FROM rust:1-trixie AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV LBUG_VERSION=0.21.2
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo test --release --locked \
    && cargo build --release --locked \
    && cp target/release/innerrag /usr/local/bin/innerrag

# ---- Runtime ----------------------------------------------------------------
FROM debian:trixie-slim AS runtime
ARG EMBED_REPO EMBED_FILE NER_REPO NER_FILE
# poppler-utils (pdftotext) and catdoc (.doc/.ppt) extract text from uploaded files.
RUN apt-get update && apt-get install -y --no-install-recommends libssl3t64 ca-certificates libatomic1 libgomp1 curl \
        poppler-utils catdoc \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 app \
    && mkdir -p /data/projects && chown -R app:app /data && chmod -R a+rwX /data
COPY --from=models /ort/libonnxruntime.so /usr/local/lib/libonnxruntime.so
COPY --from=models /models /models
COPY --from=builder /usr/local/bin/innerrag /usr/local/bin/innerrag
COPY --from=ui /ui/dist /app/ui
ENV ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so \
    HOME=/home/app \
    INNERRAG_BIND=0.0.0.0:8080 \
    INNERRAG_DATA=/data \
    INNERRAG_MODELS=/models \
    INNERRAG_UI=/app/ui \
    INNERRAG_EMBED_MODEL="${EMBED_REPO}/${EMBED_FILE}" \
    INNERRAG_NER_MODEL="${NER_REPO}/${NER_FILE}"
USER app
# Fetch the LadybugDB vector extension into /home/app/.lbdb now, so runtime needs no network.
# Readable by any uid, so the container can run as the host user (--user) on mounted folders.
RUN innerrag install-extensions && chmod a+rx /home/app && chmod -R a+rX /home/app/.lbdb
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s \
    CMD curl -fsS http://127.0.0.1:8080/api/health || exit 1
ENTRYPOINT ["innerrag"]
CMD ["serve"]

# ---- GPU runtime (NVIDIA CUDA) -----------------------------------------------
# docker build --target runtime-cuda --platform linux/amd64 -t innerrag:cuda .
# docker run --gpus all -p 8080:8080 -v ./data:/data innerrag:cuda
# Falls back to the CPU when no GPU is visible (INNERRAG_DEVICE=auto).
FROM ${CUDA_IMAGE} AS cuda-libs

FROM runtime AS runtime-cuda
USER root
COPY --from=ort-gpu /ort/ /usr/local/lib/
# Only the CUDA libraries ONNX Runtime needs; the driver (libcuda) comes from the host.
COPY --from=cuda-libs /usr/local/cuda/lib64/libcudart.so.12* /usr/local/cuda/lib64/libcublas.so.12* \
     /usr/local/cuda/lib64/libcublasLt.so.12* /usr/local/cuda/lib64/libcufft.so.11* \
     /usr/local/cuda/lib64/libcurand.so.10* /opt/cuda/lib/
COPY --from=cuda-libs /usr/lib/x86_64-linux-gnu/libcudnn*.so.9* /opt/cuda/lib/
ENV LD_LIBRARY_PATH=/usr/local/lib:/opt/cuda/lib \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    INNERRAG_DEVICE=auto
USER app

# ---- Default image: CPU -------------------------------------------------------
FROM runtime AS cpu

#!/usr/bin/env bash
# The free local model agent/transcript.json was recorded with, everything pinned:
#   model   Qwen2.5-7B-Instruct, Q4_K_M GGUF (Apache-2.0), Hugging Face Qwen/Qwen2.5-7B-Instruct-GGUF
#           at revision bb5d59e06d9551d752d08b292a50eb208b07ab1f, two files checked by sha256 below
#   runner  the llama.cpp server image, by digest; CPU only, about 4.7 GB of downloads, 6 GB of RAM
# Usage: scripts/local-model.sh start | stop
# Then:  PREAVIS_MODEL_CMD="node scripts/model-openai.mjs" npm run agent
# Nothing here is billed: the model runs on your machine; the downloads are public.
set -euo pipefail
dir="${PREAVIS_MODEL_DIR:-$HOME/.cache/preavis/qwen2.5-7b-instruct-q4_k_m}"
rev=bb5d59e06d9551d752d08b292a50eb208b07ab1f
image="ghcr.io/ggml-org/llama.cpp@sha256:41988e510b6fe0a0b5b9be00a9dbf91d9f0fa691da173d7b06d97d370329fe84"
files=(
  "qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf dfce12e3862a5283ccfb88221b48480e58745165de856439950d0f22590580db"
  "qwen2.5-7b-instruct-q4_k_m-00002-of-00002.gguf 539cf93f78e887edea1c04e2d7d8cdaca9d01dae9c9025bcb8accbe29df3d72a"
)
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

case "${1:-}" in
start)
  mkdir -p "$dir"
  for entry in "${files[@]}"; do
    name=${entry%% *} want=${entry##* }
    if [ ! -f "$dir/$name" ]; then
      echo "downloading $name" >&2
      curl -fL --retry 3 -o "$dir/$name.part" \
        "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/$rev/$name"
      mv "$dir/$name.part" "$dir/$name"
    fi
    got=$(sha256 "$dir/$name")
    [ "$got" = "$want" ] || { echo "$name: sha256 $got, expected $want" >&2; exit 1; }
    echo "ok  $name sha256 $want" >&2
  done
  docker run -d --rm --name preavis-llm -p 127.0.0.1:8080:8080 -v "$dir":/models:ro "$image" \
    -m /models/qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf --host 0.0.0.0 --port 8080 -c 4096 >/dev/null
  echo "waiting for the model server on 127.0.0.1:8080 (loading takes up to a minute)" >&2
  for _ in $(seq 1 180); do
    if curl -fs http://127.0.0.1:8080/health >/dev/null 2>&1; then
      echo 'ready. Now: PREAVIS_MODEL_CMD="node scripts/model-openai.mjs" npm run agent' >&2
      exit 0
    fi
    sleep 1
  done
  echo "the model server did not come up; docker logs preavis-llm" >&2
  exit 1
  ;;
stop) docker stop preavis-llm >/dev/null && echo "stopped" >&2 ;;
*) echo "usage: scripts/local-model.sh start | stop" >&2; exit 2 ;;
esac

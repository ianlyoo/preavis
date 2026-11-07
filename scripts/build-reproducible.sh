#!/usr/bin/env bash
# Builds the contract from a commit inside a pinned linux/amd64 Rust image, so the bytes don't depend
# on the host: Cargo's crate hashes include the host's target triple, so a native arm64 build differs
# from an x86_64 one. Embeds NEP-330 build info (image, command, commit), readable on chain through
# `contract_source_metadata`. Needs Docker (on arm64 hosts it emulates amd64, which is slower).
# Usage: scripts/build-reproducible.sh [<commit>]   (default HEAD) → target/reproducible/preavis.wasm
set -euo pipefail
cd "$(dirname "$0")/.."
image="docker.io/library/rust:1.99.1-slim-bookworm@sha256:392f221964669402bf4ae133d2a46c3ae270f64734133f4b2703d40019235c1a"
rev=$(git rev-parse "${1:-HEAD}^{commit}")
if [ -z "${1:-}" ] && ! git diff --quiet HEAD -- src Cargo.toml Cargo.lock rust-toolchain.toml .cargo scripts/build-wasm.sh; then
  echo "the contract's inputs have uncommitted changes; this builds committed source only" >&2
  exit 1
fi
out="$PWD/target/reproducible"
rm -rf "$out" && mkdir -p "$out"
echo "building commit $rev in $image" >&2
git archive "$rev" | docker run --rm -i --platform linux/amd64 \
  -e NEP330_BUILD_INFO_BUILD_ENVIRONMENT="$image" \
  -e NEP330_BUILD_INFO_BUILD_COMMAND='["bash","scripts/build-wasm.sh"]' \
  -e NEP330_BUILD_INFO_CONTRACT_PATH="" \
  -e NEP330_BUILD_INFO_SOURCE_CODE_SNAPSHOT="git+https://github.com/ianlyoo/preavis?rev=$rev" \
  -v "$out":/out "$image" \
  bash -c "mkdir /preavis && cd /preavis && tar -x && bash scripts/build-wasm.sh && cp target/wasm32-unknown-unknown/release/preavis.wasm /out/ && chown $(id -u):$(id -g) /out/preavis.wasm"
sha256sum "$out/preavis.wasm"

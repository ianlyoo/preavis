#!/usr/bin/env bash
# Builds res-comparable preavis.wasm. Paths are remapped so the bytes do not depend on where
# Cargo's registry or this checkout live. Usage: scripts/build-wasm.sh [--features <name>]
set -euo pipefail
cd "$(dirname "$0")/.."
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
export RUSTFLAGS="--cfg near --remap-path-prefix=$PWD=/preavis --remap-path-prefix=$cargo_home=/cargo"
exec cargo build --locked --release --target wasm32-unknown-unknown "$@"

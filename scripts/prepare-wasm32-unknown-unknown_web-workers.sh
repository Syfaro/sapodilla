#!/bin/sh

set -exu

export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals --cfg=web_sys_unstable_apis"

RUST_NIGHTLY_VERSION="${RUST_NIGHTLY_VERSION:-nightly}"
echo $RUST_NIGHTLY_VERSION > rust-toolchain

rustup toolchain install --target wasm32-unknown-unknown
rustup component add rust-src clippy

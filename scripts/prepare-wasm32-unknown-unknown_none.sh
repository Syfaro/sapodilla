#!/bin/sh

set -exu

rustup toolchain install stable --target wasm32-unknown-unknown
rustup component add clippy

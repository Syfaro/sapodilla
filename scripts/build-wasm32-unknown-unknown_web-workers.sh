#!/bin/sh

set -exu

export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals --cfg=web_sys_unstable_apis"

trunk build --release --features web-workers

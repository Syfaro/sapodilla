#!/bin/sh

set -exu

rustup toolchain install stable
rustup component add clippy

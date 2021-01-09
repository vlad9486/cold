#!/bin/bash
CARGO_TARGET_DIR="target/sanitizer-$1" RUSTFLAGS="-Z sanitizer=$1" \
cargo test -Zbuild-std --target x86_64-unknown-linux-gnu "${@:2}"

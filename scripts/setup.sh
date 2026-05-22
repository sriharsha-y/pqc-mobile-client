#!/usr/bin/env bash
# One-time developer setup. Installs Rust targets and helper CLIs.
# Requires: rustup (https://rustup.rs), Xcode (for iOS), Android NDK (for Android).
set -euo pipefail

if ! command -v rustup >/dev/null 2>&1; then
    echo "ERROR: rustup not found. Install from https://rustup.rs first."
    exit 1
fi

echo "==> Showing pinned toolchain"
rustup show

echo "==> Installing Android targets"
rustup target add \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android \
    i686-linux-android

echo "==> Installing iOS targets"
rustup target add \
    aarch64-apple-ios \
    aarch64-apple-ios-sim \
    x86_64-apple-ios

echo "==> Installing cargo-ndk (Android cross-compile helper)"
cargo install --locked cargo-ndk

# uniffi-bindgen is built from this crate's own [[bin]] target — no separate
# `cargo install` needed. The build scripts invoke
# `cargo run --release --bin uniffi-bindgen -- generate ...`.

cat <<'EOF'

Setup complete.

Required environment for builds:
  ANDROID_NDK_HOME = path to your Android NDK (r25c or newer recommended)
  Xcode            = installed, command-line tools selected (for iOS)

Next steps:
  ./scripts/build-android.sh    # cross-compile .so for all Android ABIs
  ./scripts/build-ios.sh        # build XCFramework for iOS
  cargo test                    # sanity-test the Rust core

EOF

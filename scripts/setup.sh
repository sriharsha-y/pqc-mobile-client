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
# `cargo install` needed. The bin is gated by the `cli` feature so the host
# tool's dep tree (clap, goblin, uniffi_bindgen) never gets linked into the
# mobile cross-compiled archives. The build scripts invoke it as:
#   cargo run --release --features cli --bin uniffi-bindgen -- generate ...
# `--features cli` is mandatory — without it cargo errors with
# "target uniffi-bindgen requires the features: cli".

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

#!/usr/bin/env bash
# Build the pqc_client Rust core. The sample's Android gradle config and iOS
# Podfile both reference the repo-root build outputs directly (target/jniLibs/
# and generated/PqcCore.xcframework respectively), so once these builds finish
# the sample picks them up — no copying step required.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SAMPLE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${SAMPLE_DIR}/../.." && pwd)"

echo "==> Building Rust core (Android ABIs)"
( cd "${REPO_ROOT}" && ./scripts/build-android.sh )

echo "==> Building Rust core (iOS XCFramework)"
( cd "${REPO_ROOT}" && ./scripts/build-ios.sh )

cat <<EOF

Builds complete. Outputs:
  Android  → ${REPO_ROOT}/target/jniLibs
  Android  → ${REPO_ROOT}/generated/kotlin
  iOS      → ${REPO_ROOT}/generated/PqcCore.xcframework
  iOS      → ${REPO_ROOT}/generated/swift/pqc.swift

Next:
  cd ${SAMPLE_DIR}
  npm install
  (cd ios && pod install)
  npx react-native run-ios     # or run-android
EOF

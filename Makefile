# Thin dispatcher over scripts/ and cargo. Targets mirror the CI commands so
# `make check` reproduces the CI 'check' job. Scripts under scripts/ stay the
# source of truth and remain runnable directly.

.DEFAULT_GOAL := help

# Mirrored from .github/workflows/check.yml so local and CI stay in lockstep.
CLIPPY     := cargo clippy --all-targets --locked -- -D warnings
CARGO_TEST := cargo test --release --locked -- --nocapture

.PHONY: help setup fmt fmt-check clippy test check audit ci android ios build sample clean

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) \
	  | awk 'BEGIN{FS=":.*## "}{printf "  \033[36m%-11s\033[0m %s\n", $$1, $$2}'

setup: ## One-time: install rustup targets + cargo-ndk
	./scripts/setup.sh

fmt: ## Format the Rust code
	cargo fmt

fmt-check: ## Check formatting without writing (CI parity)
	cargo fmt --check

clippy: ## Lint with clippy, warnings as errors (CI parity)
	$(CLIPPY)

test: ## Unit + smoke tests; hits pq.cloudflareresearch.com (CI parity)
	$(CARGO_TEST)

check: fmt-check clippy test ## Full local check — mirrors the CI 'check' job

audit: ## Supply-chain scan: cargo-audit + cargo-deny (CI parity)
	cargo audit --deny warnings
	cargo deny check

ci: check audit ## Everything CI runs on a PR (check + audit)

android: ## Cross-compile Android .so (all ABIs) + Kotlin bindings
	./scripts/build-android.sh

ios: ## Build the iOS XCFramework + Swift bindings
	./scripts/build-ios.sh

build: android ios ## Build both mobile artifacts

sample: ## Build the Rust core for the RN sample (android + ios)
	./examples/RnSample/scripts/wire-pqc.sh

clean: ## Remove build outputs (target/, generated/, android/libs/, root symlinks)
	rm -rf target generated android/libs
	rm -f PqcCore.xcframework pqc.swift

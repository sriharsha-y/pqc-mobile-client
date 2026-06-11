# Security policy

`pqc-mobile-client` is a TLS client library intended for use in mobile apps that have meaningful security requirements (banking, healthcare, etc.). Reports of vulnerabilities are taken seriously.

## Supported versions

The latest released minor version is supported with security fixes. Pre-1.0 releases follow this rule strictly — old `0.x` minors are not patched.

| Version  | Supported          |
|----------|--------------------|
| `0.11.x` | :white_check_mark: |
| `< 0.11` | :x:                |

## Reporting a vulnerability

Please **do not open a public GitHub issue** for security vulnerabilities.

Use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability) feature on this repository:

1. Go to the [Security tab](https://github.com/sriharsha-y/pqc-mobile-client/security)
2. Click **Report a vulnerability**
3. Fill in the form with as much detail as you can — reproduction steps, affected version, suggested severity, and any patch you've prototyped

Reports will be acknowledged within **5 working days**. An initial assessment (accepted / needs more info / not a vulnerability) will follow within **10 working days**.

## What's in scope

- The Rust crate `pqc_client`, its UniFFI-generated Kotlin and Swift bindings, and the published Android `.so` / iOS XCFramework artifacts.
- The example consumer code under `examples/` is for integration reference only — issues there are bugs, not vulnerabilities, unless they materially affect the security guidance the README provides.

## What's out of scope

- Vulnerabilities in upstream crates (`reqwest`, `rustls`, `rustls-post-quantum`, `aws-lc-rs`, etc.) should be reported to those projects directly. If a known upstream advisory affects this crate, please open a normal issue or PR pinning a fixed version.
- Vulnerabilities in the consumer app's wiring of the library are the consumer's responsibility, not this library's, unless caused by missing/incorrect guidance in the integration docs.
- Theoretical attacks against the underlying primitives (X25519, ML-KEM-768, AES-GCM, etc.). The crate uses standard, well-reviewed implementations via `aws-lc-rs` (built on AWS-LC, which is FIPS-capable via its `fips` feature; this build uses the non-FIPS configuration); cryptographic primitive analysis belongs upstream.

## Disclosure policy

Coordinated disclosure. Once a fix is ready and a release is cut, a GitHub Security Advisory will be published. The reporter will be credited unless they request anonymity.

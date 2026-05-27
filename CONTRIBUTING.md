# Contributing

## Commit message convention

This repo uses **[Conventional Commits](https://www.conventionalcommits.org/)** to drive automated releases via [release-please](https://github.com/googleapis/release-please).

Every commit subject must start with a type:

| Type | Triggers | Use for |
|---|---|---|
| `feat:` | Minor version bump (pre-1.0: minor, post-1.0: minor) | New user-visible features |
| `fix:` | Patch version bump | Bug fixes |
| `docs:` | No version bump | README, integration guides, rustdoc |
| `refactor:` | No version bump | Code restructuring with no behavior change |
| `perf:` | Patch version bump | Performance improvements |
| `test:` | No version bump | Adding or fixing tests |
| `ci:` | No version bump | CI / workflow changes |
| `chore:` | No version bump | Dependency bumps, build config, formatting |
| `build:` | No version bump | Build-system changes |
| `feat!:` or `fix!:` (with `!`) | Major version bump (pre-1.0: minor) | Breaking changes |

Optional scope in parentheses: `feat(tls): add cipher suite override`.

### Examples

```
feat(client): add HTTP/3 support via h3-quinn

Enables QUIC handshakes by default. Falls back to HTTP/2 on networks
that block UDP/443.

fix(tls): reject malformed base64 pin hashes early

Previously a malformed pin entry would silently disable pinning for
that hash. Now PqcConfig construction returns InvalidRequest.

ci: cache cargo registry across release workflow jobs

chore: bump rustls-post-quantum 0.2 → 0.3
```

### Commit body and footer

- Wrap body lines at 72 characters.
- Use the body to explain **why**, not what (the diff shows what).
- For breaking changes, include a `BREAKING CHANGE:` footer with migration notes.

### Commit attribution

Commits should appear authored by the human who made the change. **No** AI/model attribution (no `Co-Authored-By: Claude ...`, no "Generated with ..." trailers).

## Release flow

1. Land conventional commits on `main` (directly or via PR).
2. The `release` workflow opens/updates a release PR titled `chore(main): release X.Y.Z` containing:
   - `Cargo.toml` version bump
   - `CHANGELOG.md` entries grouped by type
3. Review the release PR. Merge it when ready.
4. release-please cuts a tag `vX.Y.Z` and a GitHub Release with the CHANGELOG entries as the body.
5. The `release` workflow then builds Android + iOS artifacts and attaches them as release assets.

## Local development

```bash
./scripts/setup.sh                 # rustup targets, cargo-ndk
# uniffi-bindgen is built from the in-tree `[[bin]]` target via
# `cargo run --features cli --bin uniffi-bindgen -- generate ...`.
# The `--features cli` gate is critical: it keeps clap / goblin /
# uniffi_bindgen out of the iOS / Android cross-compiled archives.
cargo test --release -- --test-threads=1   # unit + smoke tests (serial required: kx_tracker is process-global)
./scripts/build-android.sh         # cross-compile + Kotlin bindings
./scripts/build-ios.sh             # XCFramework + Swift bindings
cargo fmt && cargo clippy --all-targets -- -D warnings
```

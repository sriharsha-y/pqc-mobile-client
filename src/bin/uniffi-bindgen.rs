// UniFFI bindings generator binary. Invoked by scripts/build-android.sh and
// scripts/build-ios.sh as: `cargo run --bin uniffi-bindgen -- generate ...`.
//
// This is the canonical UniFFI pattern (over an external CLI install) so the
// bindgen version is locked to the same uniffi version this crate uses.

fn main() {
    uniffi::uniffi_bindgen_main()
}

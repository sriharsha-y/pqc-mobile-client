//! Android-only JNI entry point that hands the Android `Context` over to
//! `rustls-platform-verifier`, which needs it to reach KeyStore +
//! NetworkSecurityConfig at handshake time. Required exactly once per
//! process, from `Application.onCreate`, BEFORE any `PqcHttpClient` is
//! constructed.
//!
//! Without this call, the first request throws
//!   uniffi.pqc.InternalException: Expect rustls-platform-verifier to be initialized
//! at `uniffi.pqc.PqcKt.uniffiCheckCallStatus(pqc.kt)`.
//!
//! Why JNI (not UniFFI): UniFFI can't pass a live `JNIEnv` / `JObject`
//! through its FFI boundary — those are JVM-managed handles tied to the
//! calling thread. `rustls-platform-verifier::android::init_hosted`
//! *needs* exactly those handles to call back into the JVM. So we
//! expose a `extern "system"` symbol that the JVM resolves via standard
//! JNI lookup (`Java_<class>_<method>`), and Kotlin declares a matching
//! `external fun` to call it.

use jni::objects::{JClass, JObject};
use jni::JNIEnv;

/// JNI symbol name must mirror the Kotlin declaration exactly:
///   `package uniffi.pqc.android; object PqcAndroidInit {
///        @JvmStatic private external fun nativeInit(ctx: Context)
///    }`
/// → `Java_uniffi_pqc_android_PqcAndroidInit_nativeInit`. Renaming the
/// Kotlin side to `init` would clash with the public `init(ctx)` helper
/// (the idempotency guard), so we keep `nativeInit` as the FFI entry
/// point and `init` as the user-facing wrapper.
///
/// Returning `void` so a misbehaving init surfaces as a Java exception
/// (init_hosted already throws on its own failures) rather than a
/// silent `false`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_uniffi_pqc_android_PqcAndroidInit_nativeInit<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    // init_with_env captures a global reference to the Application
    // Context and a JavaVM handle. Idempotent via the crate's internal
    // OnceCell; the Kotlin side ALSO double-checks, which is
    // belt-and-suspenders.
    //
    // (We previously called `init_hosted`, which is deprecated in
    // rustls-platform-verifier 0.5.x — functionally identical, but kept
    // only for back-compat and slated for removal. See
    //   https://docs.rs/rustls-platform-verifier/0.5.3/
    //   rustls_platform_verifier/android/fn.init_hosted.html
    // The migration target is `init_with_env`.)
    if let Err(e) = rustls_platform_verifier::android::init_with_env(&mut env, context) {
        // Bubble the failure up as a Java RuntimeException so the
        // crash is loud + diagnosable in logcat. Swallowing it would
        // just defer the same error to the first request, with a
        // much less informative call site.
        //
        // If throw_new ITSELF fails we cannot signal the failure to the
        // JVM the normal way, and returning would let the Kotlin wrapper
        // mark itself initialized — turning a hard init failure into a
        // silent broken state. Abort the VM instead so the failure can
        // never be mistaken for success.
        if env
            .throw_new(
                "java/lang/RuntimeException",
                format!("rustls-platform-verifier init failed: {e:?}"),
            )
            .is_err()
        {
            env.fatal_error(format!(
                "rustls-platform-verifier init failed and the failure could not be thrown: {e:?}"
            ));
        }
    }
}

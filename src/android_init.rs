//! Android-only JNI entry point that hands the Android `Context` to
//! `rustls-platform-verifier` (needed to reach KeyStore +
//! NetworkSecurityConfig at handshake time). Must be called once per
//! process from `Application.onCreate`, before any `PqcHttpClient`.
//!
//! Why JNI, not UniFFI: UniFFI can't pass a live `JNIEnv`/`JObject`
//! across its FFI boundary (they're thread-bound JVM handles), but the
//! verifier needs exactly those to call back into the JVM. So we export
//! an `extern "system"` symbol the JVM resolves by JNI name lookup.

use jni::objects::{JClass, JObject};
use jni::JNIEnv;

/// JNI symbol must mirror the Kotlin `external fun nativeInit` exactly.
/// JNI mangling escapes the `_` in the `sriharsha_y` package segment as
/// `_1`, hence `..._sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit`.
/// Returns `void` so init failure surfaces as a Java exception, not a
/// silent `false`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    // Captures a global ref to the Application Context + a JavaVM handle.
    // Idempotent via the crate's internal OnceCell.
    if let Err(e) = rustls_platform_verifier::android::init_with_env(&mut env, context) {
        // Surface as a Java exception (loud in logcat). If throw_new
        // itself fails, abort the VM — returning would let the Kotlin
        // wrapper mark itself initialized, masking a hard init failure.
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

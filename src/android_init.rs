//! Android-only JNI entry point that hands the Android `Context` to
//! `rustls-platform-verifier` (needed to reach KeyStore +
//! NetworkSecurityConfig at handshake time). Must be called once per
//! process from `Application.onCreate`, before any `PqcHttpClient`.
//!
//! Why JNI, not UniFFI: UniFFI can't pass a live `JNIEnv`/`JObject`
//! across its FFI boundary (they're thread-bound JVM handles), but the
//! verifier needs exactly those to call back into the JVM. So we export
//! an `extern "system"` symbol the JVM resolves by JNI name lookup.

use jni::errors::{Result as JniResult, ThrowRuntimeExAndDefault};
use jni::objects::{JClass, JObject};
use jni::EnvUnowned;

/// JNI symbol must mirror the Kotlin `external fun nativeInit` exactly.
/// JNI mangling escapes the `_` in the `sriharsha_y` package segment as
/// `_1`, hence `..._sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit`.
/// Returns `void` so init failure surfaces as a Java exception, not a
/// silent `false`.
///
/// jni 0.22 native-method signature shape: the FFI param is
/// `EnvUnowned<'local>` (was `JNIEnv<'local>` in 0.21). The body is
/// wrapped in `with_env` / `resolve` so the `ThrowRuntimeExAndDefault`
/// policy converts a returned `Err` into a thrown
/// `java.lang.RuntimeException` automatically — replaces the manual
/// `env.throw_new(...)` + `env.fatal_error(...)` dance from 0.21.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    unowned_env
        .with_env(|env| -> JniResult<()> {
            // Captures a global ref to the Application Context + a
            // JavaVM handle. Idempotent via the crate's internal
            // OnceCell. The verifier's error type IS jni::errors::Error,
            // so the `?` propagates straight to the policy below.
            rustls_platform_verifier::android::init_with_env(env, context)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

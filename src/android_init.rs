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
///
/// **Why `init_with_refs` and not `init_with_env`.** `init_with_env` is
/// documented for standalone Rust Android apps where nothing else uses
/// the JVM: it captures the JavaVM + a global ref to the Context, but
/// no classloader. At handshake time the verifier looks up its bundled
/// Kotlin glue (`org.rustls.platformverifier.*`) via JNI's default
/// classloader for the calling thread — which on a Rust-spawned tokio
/// worker is the system classloader and cannot see app-bundled DEX
/// classes. The lookup then fails with `InternalException: android
/// context was not initialized`. This bites consumers with multiple
/// classloader namespaces in their process — RN 0.77 New Arch +
/// multi-DEX + RASP (TALSEC / freeRASP) is the reproduction we hit.
/// `init_with_refs` also captures the Context's classloader as a global
/// ref so the verifier can resolve its helper classes from any thread.
/// See rustls-platform-verifier PR #159 (the 0.5.1 init refactor that
/// added this variant for library use).
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    unowned_env
        .with_env(|env| -> JniResult<()> {
            // Idempotent via the verifier's internal OnceCell. The JVM +
            // both globals outlive the handshake by definition (process
            // lifetime), so the lookup at the worker-thread boundary
            // always finds the app's classloader.
            let java_vm = env.get_java_vm()?;
            let context_class = env.get_object_class(&context)?;
            let loader = context_class.get_class_loader(env)?;
            let context_global = env.new_global_ref(context)?;
            let loader_global = env.new_global_ref(loader)?;
            rustls_platform_verifier::android::init_with_refs(
                java_vm,
                context_global,
                loader_global,
            );
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

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
///
/// Uses `init_with_refs` (not the simpler `init_with_env`) so the
/// Context's classloader is captured as a global ref. Without it the
/// verifier's JNI lookup of `org.rustls.platformverifier.*` runs against
/// the calling thread's default loader — on Rust-spawned tokio workers
/// that's the system loader, which can't see app DEX in processes with
/// namespaced classloaders (RN New Arch + multi-DEX + RASP is the
/// repro). Surfaces as `InternalException: android context was not
/// initialized` at handshake time. See rustls-platform-verifier PR #159.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
) {
    unowned_env
        .with_env(|env| -> JniResult<()> {
            let java_vm = env.get_java_vm()?;
            let loader = env.get_object_class(&context)?.get_class_loader(env)?;
            rustls_platform_verifier::android::init_with_refs(
                java_vm,
                env.new_global_ref(context)?,
                env.new_global_ref(loader)?,
            );
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

//! Android-only JNI entry point. Hands the Application `Context` to the
//! two independent process-globals our Rust stack reads at runtime:
//!
//!  * `rustls-platform-verifier::android::GLOBAL` — TLS cert verification.
//!  * `ndk-context::ANDROID_CONTEXT` — read by `hickory-resolver` for DNS
//!    resolver config. Same panic string as the verifier, separate cell.
//!
//! UniFFI can't carry `JNIEnv`/`JObject` across its boundary (thread-bound
//! JVM handles), so we export an `extern "system"` symbol instead. The
//! Kotlin `PqcAndroidInit` object enforces single entry from
//! `Application.onCreate`. Behavior under split classloaders is undefined.

use jni::errors::{Result as JniResult, ThrowRuntimeExAndDefault};
use jni::objects::{Global, JClass, JObject};
use jni::EnvUnowned;
use std::ffi::c_void;
use std::sync::OnceLock;

/// Parks the Context global ref for `ndk-context`'s lifetime. ndk-context
/// stores raw pointers with no Drop hook, so the GlobalRef must outlive
/// the process — a `OnceLock` makes that structural and gives us the
/// init-once guarantee for free (upstream panics on re-init).
static NDK_CONTEXT_REF: OnceLock<Global<JObject<'static>>> = OnceLock::new();

/// JNI symbol must mirror Kotlin's `external fun nativeInit` exactly. The
/// `_` in the `sriharsha_y` package segment is JNI-mangled to `_1`, hence
/// `..._sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit` — grep for
/// `_nativeInit` when chasing `UnsatisfiedLinkError`.
///
/// We use `init_with_refs` (not `init_with_env`) so the verifier captures
/// the Context's classloader explicitly; otherwise lookups of
/// `org.rustls.platformverifier.*` miss the app's DEX in multi-classloader
/// processes (RN New Arch, multi-DEX, RASP). See
/// rustls-platform-verifier PR #159.
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

            // ndk-context: skip the JNI alloc on already-initialized re-entry.
            if NDK_CONTEXT_REF.get().is_none() {
                let context_for_ndk = env.new_global_ref(&context)?;
                let vm_ptr = java_vm.get_raw() as *mut c_void;
                NDK_CONTEXT_REF.get_or_init(|| {
                    // SAFETY: ndk-context stores both pointers for the
                    // process lifetime; java_vm is process-wide and the
                    // OnceLock keeps the Context global ref alive.
                    let context_ptr = context_for_ndk.as_raw() as *mut c_void;
                    unsafe { ndk_context::initialize_android_context(vm_ptr, context_ptr) };
                    context_for_ndk
                });
            }

            rustls_platform_verifier::android::init_with_refs(
                java_vm,
                env.new_global_ref(&context)?,
                env.new_global_ref(loader)?,
            );
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

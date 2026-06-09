//! Android-only JNI entry point that hands the Android `Context` to the
//! two independent process-globals our Rust stack reads at runtime:
//!
//!  * `rustls-platform-verifier::android::GLOBAL` — TLS cert verification
//!    (KeyStore + NetworkSecurityConfig at handshake time).
//!  * `ndk-context::ANDROID_CONTEXT` — system-context handle used by
//!    `hickory-resolver` for DNS resolver config. Same exact panic
//!    message as the verifier (`"android context was not initialized"`),
//!    same fix shape, but a SEPARATE global cell. Initializing one does
//!    not initialize the other.
//!
//! Must be called once per process from `Application.onCreate`, before
//! any `PqcHttpClient`. The Kotlin side gates with `@Volatile initialized`
//! double-checked locking.
//!
//! Why JNI, not UniFFI: UniFFI can't pass a live `JNIEnv`/`JObject`
//! across its FFI boundary (they're thread-bound JVM handles), but both
//! globals need exactly those. So we export an `extern "system"` symbol
//! the JVM resolves by JNI name lookup.

use jni::errors::{Result as JniResult, ThrowRuntimeExAndDefault};
use jni::objects::{JClass, JObject};
use jni::EnvUnowned;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

/// One-shot guard for `ndk_context::initialize_android_context`. The
/// upstream API asserts `previous.is_none()` and panics on re-init —
/// we MUST never enter that path from this `extern "system"` boundary
/// (unwinding across FFI is UB). The Kotlin side already guarantees
/// single entry, but a second JNI dlopen + classloader split could
/// produce a second call into a fresh static state; this flag is the
/// belt to the Kotlin suspenders.
static NDK_CONTEXT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// JNI symbol must mirror the Kotlin `external fun nativeInit` exactly.
/// JNI mangling escapes the `_` in the `sriharsha_y` package segment as
/// `_1`, hence `..._sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit`.
///
/// `rustls-platform-verifier::init_with_refs` (not the simpler
/// `init_with_env`) captures the Context's classloader explicitly so
/// the verifier's lookup of `org.rustls.platformverifier.*` resolves
/// against the app's DEX in multi-classloader processes (RN New Arch +
/// multi-DEX + RASP). See rustls-platform-verifier PR #159.
///
/// `ndk_context::initialize_android_context` is the same pattern for the
/// ndk-context global that `hickory-resolver` reads — independent cell,
/// same panic message if uninitialized. The Context global ref is leaked
/// via `mem::forget` so the raw JNI pointer remains valid for the
/// process lifetime (ndk-context stores raw pointers, no Drop hook).
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
            let context_for_verifier = env.new_global_ref(&context)?;
            let loader_global = env.new_global_ref(loader)?;

            // ndk-context: prepare BEFORE init_with_refs takes ownership of
            // java_vm. The compare_exchange runs AFTER the fallible
            // new_global_ref so a transient JNI failure can't latch the gate
            // on without the upstream init actually succeeding — otherwise
            // a retried nativeInit would skip this block forever and the
            // very bug we're fixing would re-emerge.
            let vm_ptr = java_vm.get_raw() as *mut c_void;
            let context_for_ndk = env.new_global_ref(&context)?;
            if NDK_CONTEXT_INITIALIZED
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let context_ptr = context_for_ndk.as_raw() as *mut c_void;
                // SAFETY: vm_ptr and context_ptr outlive the process —
                // java_vm itself is a process-wide handle and we mem::forget
                // the global ref so the JNI global reference is never released.
                unsafe { ndk_context::initialize_android_context(vm_ptr, context_ptr) };
                std::mem::forget(context_for_ndk);
            }
            // (If we lost the race, context_for_ndk drops naturally and the
            // JNI global ref is released — no leak.)

            rustls_platform_verifier::android::init_with_refs(
                java_vm,
                context_for_verifier,
                loader_global,
            );
            Ok(())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

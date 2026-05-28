package io.github.sriharsha_y.pqc.android

import android.content.Context

/**
 * One-time Android-side initialization for `pqc_client`.
 *
 * `rustls-platform-verifier` (the cert-chain verifier under our TLS
 * stack) holds a JVM reference to the Application `Context` so it can
 * reach `KeyStore`, `NetworkSecurityConfig`, and revocation lookups
 * at handshake time. The Rust side cannot fish that out on its own
 * without going through JNI, so this object hands it over.
 *
 * **Call exactly once, from `Application.onCreate`, BEFORE constructing
 * any [io.github.sriharsha_y.pqc.PqcHttpClient]**:
 *
 * ```kotlin
 * class MyApp : Application() {
 *   override fun onCreate() {
 *     super.onCreate()
 *     io.github.sriharsha_y.pqc.android.PqcAndroidInit.init(this)
 *     // ... PqcHttpClient may now be constructed
 *   }
 * }
 * ```
 *
 * Skipping this throws on the first request:
 *   `io.github.sriharsha_y.pqc.InternalException: Expect rustls-platform-verifier to be initialized`
 *
 * Idempotent at the Kotlin level — a redundant call short-circuits
 * before crossing into Rust.
 *
 * iOS has no equivalent: Apple's Security framework is process-wide
 * and discovered via `dlopen`, so iOS consumers do nothing extra.
 */
object PqcAndroidInit {
    @Volatile private var initialized = false

    init {
        // The native shim lives in libpqc_client (built by
        // scripts/build-android.sh). UniFFI's own bindings call
        // System.loadLibrary on first use of any FFI function, but
        // since we may be called BEFORE any UniFFI surface is
        // touched (and intentionally so), force-load it here.
        System.loadLibrary("pqc_client")
    }

    /**
     * Hand the Android Context to rustls-platform-verifier.
     * Pass the Application context, NOT an Activity context — the
     * verifier holds the reference for the lifetime of the process.
     */
    @JvmStatic
    fun init(context: Context) {
        if (initialized) return
        synchronized(this) {
            if (initialized) return
            // Always pass the Application context. If a caller hands
            // us an Activity by mistake, applicationContext yields
            // the right long-lived reference.
            nativeInit(context.applicationContext)
            initialized = true
        }
    }

    /**
     * Resolves to `Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit`
     * in src/android_init.rs. The JVM derives the JNI symbol from the
     * fully-qualified class + method name; the `_` in `sriharsha_y` is
     * JNI-mangled to `_1`. Grep for `_nativeInit` when debugging
     * UnsatisfiedLinkError, not `_init`.
     */
    @JvmStatic
    private external fun nativeInit(context: Context)
}

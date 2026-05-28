package io.github.sriharsha_y.pqc.android

import android.content.Context

/**
 * One-time Android-side initialization for `pqc_client`.
 *
 * `rustls-platform-verifier` holds a JVM reference to the Application
 * `Context` to reach `KeyStore`, `NetworkSecurityConfig`, and revocation
 * lookups at handshake time. Rust can only obtain it through JNI, so this
 * object hands it over.
 *
 * **Call exactly once, from `Application.onCreate`, BEFORE constructing
 * any [io.github.sriharsha_y.pqc.PqcHttpClient]**:
 *
 * ```kotlin
 * class MyApp : Application() {
 *   override fun onCreate() {
 *     super.onCreate()
 *     io.github.sriharsha_y.pqc.android.PqcAndroidInit.init(this)
 *   }
 * }
 * ```
 *
 * Skipping this throws on the first request:
 *   `io.github.sriharsha_y.pqc.InternalException: Expect rustls-platform-verifier to be initialized`
 *
 * Idempotent — a redundant call short-circuits before crossing into Rust.
 * iOS has no equivalent: Apple's Security framework is process-wide.
 */
object PqcAndroidInit {
    @Volatile private var initialized = false

    init {
        // We may be called BEFORE any UniFFI surface is touched (which is
        // when UniFFI would otherwise loadLibrary), so force-load it here.
        System.loadLibrary("pqc_client")
    }

    /**
     * Hand the Android Context to rustls-platform-verifier. Pass the
     * Application context, NOT an Activity — the verifier holds the
     * reference for the lifetime of the process.
     */
    @JvmStatic
    fun init(context: Context) {
        if (initialized) return
        synchronized(this) {
            if (initialized) return
            // applicationContext yields the long-lived reference even if a
            // caller hands us an Activity by mistake.
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

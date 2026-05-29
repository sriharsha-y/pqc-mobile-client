package io.github.sriharsha_y.pqcsample

import android.app.Application
import io.github.sriharsha_y.pqc.android.PqcAndroidInit

/**
 * Hands the Application context to rustls-platform-verifier exactly once,
 * before any [io.github.sriharsha_y.pqc.PqcHttpClient] is constructed.
 *
 * Skipping this throws on the first request:
 *   io.github.sriharsha_y.pqc.InternalException:
 *   Expect rustls-platform-verifier to be initialized
 */
class SampleApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        PqcAndroidInit.init(this)
    }
}

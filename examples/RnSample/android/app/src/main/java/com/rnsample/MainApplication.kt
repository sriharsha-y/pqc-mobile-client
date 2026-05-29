package com.rnsample

import android.app.Application
import android.util.Log
import com.facebook.react.PackageList
import com.facebook.react.ReactApplication
import com.facebook.react.ReactHost
import com.facebook.react.ReactNativeHost
import com.facebook.react.ReactPackage
import com.facebook.react.defaults.DefaultNewArchitectureEntryPoint.load
import com.facebook.react.defaults.DefaultReactHost.getDefaultReactHost
import com.facebook.react.defaults.DefaultReactNativeHost
import com.facebook.react.modules.network.OkHttpClientFactory
import com.facebook.react.modules.network.OkHttpClientProvider
import com.facebook.react.modules.network.ReactCookieJarContainer
import com.facebook.react.soloader.OpenSourceMergedSoMapping
import com.facebook.soloader.SoLoader
import okhttp3.OkHttpClient
import io.github.sriharsha_y.pqc.PqcConfig
import io.github.sriharsha_y.pqc.PqcHttpClient
import io.github.sriharsha_y.pqc.RedirectPolicy
import io.github.sriharsha_y.pqc.android.PqcAndroidInit
import java.util.concurrent.TimeUnit

class MainApplication : Application(), ReactApplication {

  override val reactNativeHost: ReactNativeHost =
      object : DefaultReactNativeHost(this) {
        override fun getPackages(): List<ReactPackage> =
            PackageList(this).packages.apply {
              // Packages that cannot be autolinked yet can be added manually here, for example:
              // add(MyReactNativePackage())
            }

        override fun getJSMainModuleName(): String = "index"

        override fun getUseDeveloperSupport(): Boolean = BuildConfig.DEBUG

        override val isNewArchEnabled: Boolean = BuildConfig.IS_NEW_ARCHITECTURE_ENABLED
        override val isHermesEnabled: Boolean = BuildConfig.IS_HERMES_ENABLED
      }

  override val reactHost: ReactHost
    get() = getDefaultReactHost(applicationContext, reactNativeHost)

  override fun onCreate() {
    // Install the PQC OkHttpClient BEFORE super.onCreate() / any network use.
    // Per react-native#34789, setOkHttpClientFactory is read lazily and
    // silently no-ops if NetworkingModule already built its client.
    installPqcOkHttpFactory()

    super.onCreate()
    SoLoader.init(this, OpenSourceMergedSoMapping)
    if (BuildConfig.IS_NEW_ARCHITECTURE_ENABLED) {
      load()
    }
  }

  private fun installPqcOkHttpFactory() {
    // Must run BEFORE constructing PqcHttpClient: the constructor builds the
    // TLS config, which calls the verifier, which requires this init.
    // Otherwise the first request throws
    //   io.github.sriharsha_y.pqc.InternalException: Expect rustls-platform-verifier to be initialized
    PqcAndroidInit.init(this)

    // Shared config differing only in enablePostQuantum; the sample keeps
    // both clients so the UI can toggle PQC (the flag is fixed at construction).
    fun config(enablePqc: Boolean) = PqcConfig(
      // Empty = pinning disabled. A real banking app should populate with
      // base64(SHA-256(SPKI)) for the production cert + at least one backup.
      pinnedCertSha256 = emptyList(),
      enablePostQuantum = enablePqc,
      defaultTimeoutMs = 15_000UL,
      // null = built-in defaults (10s connect, 16 MiB body cap). Set these
      // explicitly in production so they survive a defaults change.
      connectTimeoutMs = null,
      maxBodyBytes = null,
      // Banking clients should NOT auto-attach Set-Cookie across endpoints
      // (session-leak vector); round-trip cookies via headers when needed.
      enableCookies = false,
      // Identify the app to Akamai Bot Manager / bank WAFs.
      userAgent = "RnSample/0.3.1 (pqc-mobile-client)",
      // Refuse cross-origin redirects so the original handshake's pin / PQ
      // guarantees can't be silently dropped by a 3xx to another host.
      redirectPolicy = RedirectPolicy.SameOriginOnly,
      // Opt-in RFC 9111 response cache (off here). To enable, set
      // enableCache = true and pass cacheDir = cacheDir.absolutePath.
      enableCache = false,
      cacheDir = null,
      maxCacheBytes = null,
    )

    val pqcClient: PqcHttpClient
    val classicalClient: PqcHttpClient
    try {
      pqcClient = PqcHttpClient(config(enablePqc = true))
      classicalClient = PqcHttpClient(config(enablePqc = false))
    } catch (t: Throwable) {
      Log.e(TAG, "PqcHttpClient construction failed; falling back to default OkHttp", t)
      return
    }

    OkHttpClientProvider.setOkHttpClientFactory(OkHttpClientFactory {
      OkHttpClient.Builder()
        .connectTimeout(0, TimeUnit.MILLISECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .writeTimeout(0, TimeUnit.MILLISECONDS)
        .cookieJar(ReactCookieJarContainer())
        .addInterceptor(PqcInterceptor(pqcClient, classicalClient))    // MUST be last
        .build()
    })
    Log.i(TAG, "PQC OkHttp factory installed")
  }

  companion object {
    private const val TAG = "RnSample.PQC"
  }
}

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
import uniffi.pqc.PqcConfig
import uniffi.pqc.PqcHttpClient
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
    // Install the PQC-backed OkHttpClient BEFORE super.onCreate() and before any
    // RN module touches the network. Per react-native#34789, setOkHttpClientFactory
    // is read lazily on first network call and silently no-ops if NetworkingModule
    // has already constructed its client.
    installPqcOkHttpFactory()

    super.onCreate()
    SoLoader.init(this, OpenSourceMergedSoMapping)
    if (BuildConfig.IS_NEW_ARCHITECTURE_ENABLED) {
      load()
    }
  }

  private fun installPqcOkHttpFactory() {
    val pqc = try {
      PqcHttpClient(
        PqcConfig(
          // Empty list = pinning disabled. For a real banking app, populate with
          // base64(SHA-256(SPKI)) for the production cert + at least one backup.
          pinnedCertSha256 = emptyList(),
          enablePostQuantum = true,
          enableHttp3 = false,
          defaultTimeoutMs = 15_000UL,
        )
      )
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
        .addInterceptor(PqcInterceptor(pqc))    // MUST be last
        .build()
    })
    Log.i(TAG, "PQC OkHttp factory installed")
  }

  companion object {
    private const val TAG = "RnSample.PQC"
  }
}

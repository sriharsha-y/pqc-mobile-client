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

    // prewarm() forces PqcHttpClient construction so a misconfigured build
    // (bad pin, missing native library) surfaces here. On failure, return
    // without installing the factory — RN falls back to its default
    // OkHttpClient (classical TLS) and the app stays online.
    val pqcInterceptor = try {
      RnSamplePqcInterceptor(applicationContext).also { it.prewarm() }
    } catch (t: Throwable) {
      Log.e("RnSample.PQC", "PqcHttpClient construction failed; falling back to default OkHttp", t)
      return
    }

    OkHttpClientProvider.setOkHttpClientFactory(OkHttpClientFactory {
      OkHttpClient.Builder()
        .connectTimeout(0, TimeUnit.MILLISECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .writeTimeout(0, TimeUnit.MILLISECONDS)
        .cookieJar(ReactCookieJarContainer())
        .addInterceptor(pqcInterceptor)    // MUST be last
        .build()
    })
  }
}

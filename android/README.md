# Android consumption guide

`pqc_client` on Android, consumed from:

- **A native Android app** using OkHttp / Retrofit / Ktor / raw `HttpURLConnection` (§3, §4)
- **A React Native Android app** (§5)
- **Direct from Kotlin/Java without any HTTP framework** (§6)

The Rust core, the `.so` files, and the generated Kotlin bindings are the same regardless of consumer.

## 1. Build outputs

After `./scripts/build-android.sh` at the repo root:

```
target/jniLibs/
├── arm64-v8a/libpqc_client.so       (~3–4 MB stripped, opt-level=z, LTO)
├── armeabi-v7a/libpqc_client.so
└── x86_64/libpqc_client.so          (emulator)

generated/kotlin/
└── uniffi/pqc/
    └── pqc.kt                     (UniFFI-generated Kotlin bindings)
```

## 2. Packaging options

### A — Ship as an AAR (recommended for distribution)

A thin Android library module bundles the `.so` files and the generated Kotlin, then publishes to internal Artifactory. Minimal `build.gradle.kts`:

```kotlin
plugins {
    id("com.android.library")
    kotlin("android")
}

android {
    namespace = "com.yourorg.pqc"
    compileSdk = 35
    defaultConfig { minSdk = 29 }
    sourceSets["main"].apply {
        jniLibs.srcDir("../target/jniLibs")
        java.srcDir("../generated/kotlin")
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")           // UniFFI runtime
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.1")
}
```

Publish to Artifactory as `com.yourorg.pqc:pqc-mobile-client:0.1.0`.

### B — Drop straight into the consumer Android project

For early iteration without publishing an AAR, copy:
- `target/jniLibs/*` → `app/src/main/jniLibs/`
- `generated/kotlin/uniffi/pqc/pqc.kt` → `app/src/main/java/uniffi/pqc/pqc.kt`

Add the JNA + coroutines deps to the consumer's `build.gradle`. Works but won't survive an Expo `prebuild`.

## 3. Native Android — OkHttp / Retrofit / Ktor

OkHttp's `Interceptor` is the universal hook. Anything built on OkHttp (Retrofit, Ktor with the OkHttp engine, Apollo with OkHttp) inherits the swap.

```kotlin
import okhttp3.Interceptor
import okhttp3.Response
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.ResponseBody.Companion.toResponseBody
import kotlinx.coroutines.runBlocking
import uniffi.pqc.*

class PqcInterceptor(private val client: PqcHttpClient) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()
        val pqcReq = HttpRequest(
            method = req.method.toPqcMethod(),
            url = req.url.toString(),
            headers = req.headers.toMap(),
            body = req.body?.let { okio.Buffer().also { b -> it.writeTo(b) }.readByteArray() },
            timeoutMs = null,
        )
        val pqcResp = runBlocking { client.request(pqcReq) }
        val ct = pqcResp.headers["content-type"]?.firstOrNull()?.toMediaTypeOrNull()
        val builder = Response.Builder()
            .request(req)
            .protocol(okhttp3.Protocol.HTTP_2)
            .code(pqcResp.status.toInt())
            .message("")
            .body(pqcResp.body.toResponseBody(ct))
        pqcResp.headers.forEach { (k, vs) -> vs.forEach { builder.addHeader(k, it) } }
        return builder.build()
    }

    private fun String.toPqcMethod() = when (uppercase()) {
        "GET" -> HttpMethod.GET; "POST" -> HttpMethod.POST; "PUT" -> HttpMethod.PUT
        "DELETE" -> HttpMethod.DELETE; "PATCH" -> HttpMethod.PATCH
        "HEAD" -> HttpMethod.HEAD; "OPTIONS" -> HttpMethod.OPTIONS
        else -> error("unsupported HTTP method: $this")
    }
}

// Installation
val pqc = PqcHttpClient(PqcConfig(
    pinnedCertSha256 = CertPins.SPKI_SHA256,   // see §10 for how to compute
    enablePostQuantum = true,
    enableHttp3 = false,
    defaultTimeoutMs = 15_000UL,
))

val okHttp = OkHttpClient.Builder()
    .addInterceptor(authHeaderInterceptor)        // runs first
    .addInterceptor(observabilityInterceptor)
    .addInterceptor(PqcInterceptor(pqc))          // MUST be last
    .build()

// Retrofit / Ktor / Apollo sit on top of okHttp unchanged.
val retrofit = Retrofit.Builder()
    .baseUrl("https://api.example.com/")
    .client(okHttp)
    .addConverterFactory(MoshiConverterFactory.create())
    .build()
```

## 4. Native Android — `HttpURLConnection` or no framework

`HttpURLConnection` does not expose an interceptor model. The clean answer is to skip it and call `PqcHttpClient` directly (see §6). If the consumer code must keep `HttpURLConnection` semantics, wrap `PqcHttpClient` behind a thin `HttpURLConnection`-shaped facade — possible but ~150 LOC of glue, not provided here.

## 5. React Native Android

The RN networking module reads its `OkHttpClient` from `OkHttpClientProvider`. The factory **must** be installed before `super.onCreate()` to avoid [RN issue #34789](https://github.com/facebook/react-native/issues/34789) where a late swap silently no-ops.

```kotlin
// android/app/src/main/java/com/yourapp/MainApplication.kt
import com.facebook.react.modules.network.OkHttpClientProvider
import com.facebook.react.modules.network.OkHttpClientFactory
import com.facebook.react.modules.network.ReactCookieJarContainer
import uniffi.pqc.*
import java.util.concurrent.TimeUnit

class MainApplication : Application(), ReactApplication {
    private val pqc by lazy {
        PqcHttpClient(PqcConfig(
            pinnedCertSha256 = CertPins.SPKI_SHA256,
            enablePostQuantum = true,
            enableHttp3 = false,
            defaultTimeoutMs = 15_000UL,
        ))
    }

    override fun onCreate() {
        OkHttpClientProvider.setOkHttpClientFactory(OkHttpClientFactory {
            OkHttpClient.Builder()
                .connectTimeout(0, TimeUnit.MILLISECONDS)
                .readTimeout(0, TimeUnit.MILLISECONDS)
                .writeTimeout(0, TimeUnit.MILLISECONDS)
                .cookieJar(ReactCookieJarContainer())
                .addInterceptor(authHeaderInterceptor)
                .addInterceptor(observabilityInterceptor)
                .addInterceptor(PqcInterceptor(pqc))      // MUST be last
                .build()
        })
        super.onCreate()
        SoLoader.init(this, false)
        // ... rest of RN init
    }
}
```

The `PqcInterceptor` class is identical to the native case (§3).

## 6. Direct use — no HTTP framework

For new code paths that don't have an existing HTTP client to swap, use `PqcHttpClient` directly:

```kotlin
import uniffi.pqc.*
import kotlinx.coroutines.runBlocking

val pqc = PqcHttpClient(PqcConfig(
    pinnedCertSha256 = emptyList(),
    enablePostQuantum = true,
    enableHttp3 = false,
    defaultTimeoutMs = 10_000UL,
))

// Suspending call from a coroutine
suspend fun fetchBalance(): String {
    val resp = pqc.request(HttpRequest(
        method = HttpMethod.GET,
        url = "https://api.bank.example/accounts/123/balance",
        headers = mapOf("Authorization" to "Bearer $token"),
        body = null,
        timeoutMs = null,
    ))
    return String(resp.body, Charsets.UTF_8)
}

// Blocking adapter for Java / legacy code
fun fetchBalanceBlocking() = runBlocking { fetchBalance() }
```

`PqcHttpClient` is a UniFFI-generated class; `request(...)` is a `suspend fun` in Kotlin and a `Future`-style call in Java (UniFFI's Java compatibility layer).

## 7. ProGuard / R8

UniFFI uses JNI; keep the generated bindings and JNA's native methods:

```proguard
# proguard-rules.pro
-keep class uniffi.pqc.** { *; }
-keep class com.sun.jna.** { *; }
-keepclasseswithmembers class * { native <methods>; }
```

## 8. ABI strategy

Ship as an App Bundle so each device only downloads its ABI's `.so`. `arm64-v8a` is the only required ABI for modern devices; `armeabi-v7a` is optional (small 32-bit ARM tail in some emerging markets); `x86_64` is for emulators.

## 9. Verification

Debug-build verification call:

```kotlin
val resp = pqc.request(HttpRequest(
    method = HttpMethod.GET,
    url = "https://pq.cloudflareresearch.com/",
    headers = emptyMap(), body = null, timeoutMs = 5000UL,
))
android.util.Log.i("PQC", "negotiated group: ${resp.negotiatedNamedGroup}")
```

For production verification use Wireshark on a USB-tethered device — filter `tls.handshake.type == 1` and inspect the `key_share` extension for group `0x11EC`. ClientHello is unencrypted; no decryption needed.

For fleet-level telemetry, query Akamai DataStream 2 for the negotiated named group per request, broken down by client OS and app version.

## 10. SPKI cert pinning — how to compute hashes

`PqcConfig.pinnedCertSha256` takes a list of base64-encoded SHA-256 hashes of the **Subject Public Key Info** (SPKI) — the same format used by RFC 7469 and Cronet's `addPublicKeyPins`. Empty list disables pinning.

Compute from a live server:

```sh
openssl s_client -servername api.example.com -connect api.example.com:443 < /dev/null 2>/dev/null \
  | openssl x509 -pubkey -noout \
  | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary \
  | base64
```

Compute from a cert file:

```sh
openssl x509 -in cert.pem -pubkey -noout \
  | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary \
  | base64
```

**Always pin at least two hashes** — the current leaf SPKI and one backup (e.g., a future leaf or an intermediate CA). Set the pin set's effective expiry to ≥ 12 months out, with a rotation playbook documented for cert renewal.

The verifier layers SPKI pinning **on top of** the system trust verification — both must pass. If either fails, the handshake is rejected with `PqcError.PinningFailure`.

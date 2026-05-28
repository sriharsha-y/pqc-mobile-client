# Android consumption guide

`pqc_client` on Android, consumed from:

- **A native Android app** using OkHttp / Retrofit / Ktor / raw `HttpURLConnection` (Sections 3 and 4)
- **A React Native Android app** (Section 5)
- **Direct from Kotlin/Java without any HTTP framework** (Section 6)

The Rust core, the `.so` files, and the generated Kotlin bindings are the same regardless of consumer.

> **Package note (upgraders):** the Kotlin bindings are published under
> `io.github.sriharsha_y.pqc` (matching the Maven group / AAR namespace).
> Earlier releases used UniFFI's default `uniffi.pqc`. If you are upgrading
> from such a release, update your imports (`uniffi.pqc.*` →
> `io.github.sriharsha_y.pqc.*`) and any proguard keep rule
> (`-keep class uniffi.pqc.** { *; }` → `io.github.sriharsha_y.pqc.**`).
> iOS/Swift consumers are unaffected (the binding is the `PqcCore` module).

## 1. Build outputs

> **Note on regenerating bindings manually.** The build script invokes
> `cargo run --release --features cli --bin uniffi-bindgen -- generate ...`.
> The `--features cli` flag is mandatory — the uniffi-bindgen binary is
> gated behind a `cli` cargo feature so its dep tree (clap, goblin,
> uniffi_bindgen itself) never gets linked into the mobile cross-compiled
> archive. Running `cargo run --bin uniffi-bindgen ...` without the flag
> errors with `target uniffi-bindgen requires the features: cli`.

After `make android` at the repo root:

```
target/jniLibs/
├── arm64-v8a/libpqc_client.so       (~3–4 MB stripped, opt-level=z, LTO)
├── armeabi-v7a/libpqc_client.so
└── x86_64/libpqc_client.so          (emulator)

generated/kotlin/
└── io/github/sriharsha_y/pqc/
    └── pqc.kt                     (UniFFI-generated Kotlin bindings)
```

## 2. Packaging options

### A — Maven Central (recommended)

The library publishes to Maven Central on every release under the coordinates `io.github.sriharsha-y:pqc-mobile-client`. In the consumer's `build.gradle.kts`:

```kotlin
dependencies {
    implementation("io.github.sriharsha-y:pqc-mobile-client:0.5.1") // x-release-please-version
}
```

That pulls the AAR (with `arm64-v8a`, `armeabi-v7a`, `x86_64` slices), the Kotlin bindings, and the JNA + kotlinx-coroutines transitive dependencies in one declaration. No local cargo build, no manual `.so` vendoring.

The published AAR is **self-contained**: it bundles the `rustls-platform-verifier` Kotlin glue (`org.rustls.platformverifier.*`) under its own `libs/` entry. Consumers do **not** need to add a separate Maven repository for it. Upstream ships those classes only as a vendored AAR inside the Cargo registry — `scripts/build-android.sh` extracts that jar and AGP embeds it into our AAR at publish time.

### B — Tarball from the GitHub Release (no Maven access)

For consumers behind corporate proxies that block Maven Central, or for early integration before Maven Central publication is ready, download `pqc-mobile-client-X.Y.Z-android.tar.gz` from the release page and unpack:

- `jniLibs/*` → `app/src/main/jniLibs/`
- `kotlin/io/github/sriharsha_y/pqc/pqc.kt` → `app/src/main/java/io/github/sriharsha_y/pqc/pqc.kt`
- `libs/rustls-platform-verifier-*.jar` → `app/libs/`  (vendored Kotlin glue; without it the first TLS handshake throws `NoClassDefFoundError: org.rustls.platformverifier.CertificateVerifier`)

Add the JNA + coroutines deps to the consumer's `build.gradle` manually, plus `implementation(fileTree("libs") { include("*.jar") })` so AGP picks up the platform-verifier jar. Works but won't survive an Expo `prebuild`.

### C — Local Gradle module (development)

If you're hacking on `pqc-mobile-client` itself, build locally:

```bash
./scripts/build-android.sh
cd android && ./gradlew assembleRelease
```

The AAR lands at `android/build/outputs/aar/pqc-mobile-client-release.aar`. Reference it from the consumer with `implementation(files("path/to/pqc-mobile-client-release.aar"))` plus the JNA + coroutines deps. The wrapper pins Gradle 8.7 to match CI; the parent build's `preBuild` task fails with a helpful error if you forgot to run `scripts/build-android.sh` first (it extracts the rustls-platform-verifier jar into `android/libs/` that the AAR bundles).

## 3. Native Android — OkHttp / Retrofit / Ktor

OkHttp's `Interceptor` is the universal hook. Anything built on OkHttp (Retrofit, Ktor with the OkHttp engine, Apollo with OkHttp) inherits the swap.

```kotlin
import okhttp3.Interceptor
import okhttp3.Response
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.ResponseBody.Companion.toResponseBody
import kotlinx.coroutines.runBlocking
import io.github.sriharsha_y.pqc.*

class PqcInterceptor(private val client: PqcHttpClient) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()
        val pqcReq = HttpRequest(
            method = req.method.toPqcMethod(),
            url = req.url.toString(),
            headers = req.headers.toMultimap(),
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

// Installation. The PqcHttpClient constructor throws on malformed config
// (e.g. bad base64 in pinnedCertSha256) — wrap in try/catch in production.
val pqc = PqcHttpClient(PqcConfig(
    pinnedCertSha256 = CertPins.SPKI_SHA256,   // see Section 10 for how to compute
    enablePostQuantum = true,
    defaultTimeoutMs = 15_000UL,
    connectTimeoutMs = null,                   // 10s default
    maxBodyBytes = null,                       // 16 MiB default
    enableCookies = false,                     // banking: no auto cookie jar
    userAgent = "MyApp/1.0",                   // identify to bank WAF / Akamai
    redirectPolicy = RedirectPolicy.SameOriginOnly,
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

`HttpURLConnection` does not expose an interceptor model. The clean answer is to skip it and call `PqcHttpClient` directly (see Section 6). If the consumer code must keep `HttpURLConnection` semantics, wrap `PqcHttpClient` behind a thin `HttpURLConnection`-shaped facade — possible but ~150 LOC of glue, not provided here.

## 5. React Native Android

The RN networking module reads its `OkHttpClient` from `OkHttpClientProvider`. The factory **must** be installed before `super.onCreate()` to avoid [RN issue #34789](https://github.com/facebook/react-native/issues/34789) where a late swap silently no-ops.

```kotlin
// android/app/src/main/java/com/yourapp/MainApplication.kt
import com.facebook.react.modules.network.OkHttpClientProvider
import com.facebook.react.modules.network.OkHttpClientFactory
import com.facebook.react.modules.network.ReactCookieJarContainer
import io.github.sriharsha_y.pqc.*
import java.util.concurrent.TimeUnit

class MainApplication : Application(), ReactApplication {
    private val pqc by lazy {
        PqcHttpClient(PqcConfig(
            pinnedCertSha256 = CertPins.SPKI_SHA256,
            enablePostQuantum = true,
            defaultTimeoutMs = 15_000UL,
            connectTimeoutMs = null,
            maxBodyBytes = null,
            enableCookies = false,
            userAgent = "MyApp/1.0",
            redirectPolicy = RedirectPolicy.SameOriginOnly,
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

The `PqcInterceptor` class is identical to the native case (Section 3).

## 6. Direct use — no HTTP framework

For new code paths that don't have an existing HTTP client to swap, use `PqcHttpClient` directly:

```kotlin
import io.github.sriharsha_y.pqc.*
import kotlinx.coroutines.runBlocking

val pqc = PqcHttpClient(PqcConfig(
    pinnedCertSha256 = emptyList(),
    enablePostQuantum = true,
    defaultTimeoutMs = 10_000UL,
    connectTimeoutMs = null,
    maxBodyBytes = null,
    enableCookies = false,
    userAgent = "MyApp/1.0",
    redirectPolicy = RedirectPolicy.SameOriginOnly,
))

// Suspending call from a coroutine
suspend fun fetchBalance(): String {
    val resp = pqc.request(HttpRequest(
        method = HttpMethod.GET,
        url = "https://api.bank.example/accounts/123/balance",
        headers = mapOf("Authorization" to listOf("Bearer $token")),
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
-keep class io.github.sriharsha_y.pqc.** { *; }
-keep class com.sun.jna.** { *; }
-keepclasseswithmembers class * { native <methods>; }
```

## 8. ABI strategy

Ship as an App Bundle so each device only downloads its ABI's `.so`. `arm64-v8a` is the only required ABI for modern devices; `armeabi-v7a` is optional (small 32-bit ARM tail in some emerging markets); `x86_64` is for emulators.

## 9. Verification

Debug-build verification call. To confirm the negotiated key exchange, read the **server's** report from Cloudflare's `/cdn-cgi/trace` (the `kex=` line) — `HttpResponse` deliberately does not expose the group, because it is a per-connection property the client can only observe via a racy process-global (see the `HttpResponse` doc in `src/pqc.udl`):

```kotlin
val resp = pqc.request(HttpRequest(
    method = HttpMethod.GET,
    url = "https://pq.cloudflareresearch.com/cdn-cgi/trace",
    headers = emptyMap<String, List<String>>(), body = null, timeoutMs = 5000UL,
))
val kex = String(resp.body).lineSequence()
    .firstOrNull { it.startsWith("kex=") }?.removePrefix("kex=")
android.util.Log.i("PQC", "kex=$kex alpn=${resp.negotiatedProtocol}")
// kex == "X25519MLKEM768" → post-quantum; "X25519" → classical.
// `negotiatedProtocol` is per-request ("h2", "http/1.1").
```

For production verification use Wireshark on a USB-tethered device — filter `tls.handshake.type == 1` and inspect the `key_share` extension for group `0x11EC`. ClientHello is unencrypted; no decryption needed.

For fleet-level telemetry, query Akamai DataStream 2 (or your edge's TLS observability) for the negotiated named group per request, broken down by client OS and app version.

## 10. SPKI cert pinning — how to compute hashes

`PqcConfig.pinnedCertSha256` takes a list of base64-encoded SHA-256 hashes of a certificate's **Subject Public Key Info** (SPKI) — the same format used by RFC 7469 and Cronet's `addPublicKeyPins`. Both standard (`+`/`/`) and URL-safe (`-`/`_`) alphabets are accepted, with or without padding. Empty list disables pinning.

A pin matches if **any certificate in the server's chain — leaf or intermediate — has a matching SPKI hash** (the leaf must still parse), the same semantics as OkHttp's `CertificatePinner` and Android's `NetworkSecurityConfig` `<pin-set>`.

Compute from a live server (use `-showcerts` to also see the intermediate, the 2nd cert in the chain):

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

**Recommended: pin your issuing intermediate CA.** Its key has a multi-year lifespan and is far more specific than a public root, so the leaf can rotate freely (CA-forced reissue, ACME renewal) without an app update. Pinning the leaf alone is the most fragile option — a single reissue without a matching pin already shipped will brick the app.

**Always pin at least two hashes** (e.g. the current intermediate + a backup intermediate, or a pre-deployed next leaf). Set the pin set's effective expiry to ≥ 12 months out, with a rotation playbook documented for cert renewal. **Never pin a public root** (e.g. ISRG Root X1): every cert that root issues would satisfy the pin, defeating the guarantee.

The verifier layers SPKI pinning **on top of** the system trust verification — both must pass. If either fails, the handshake is rejected with `PqcError.PinningFailure`.

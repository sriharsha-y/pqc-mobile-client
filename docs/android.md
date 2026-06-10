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

> Regenerating bindings manually requires `--features cli` — the `uniffi-bindgen` binary is gated behind it so its deps (clap, goblin, uniffi_bindgen) stay out of the mobile archive.

After `make android` at the repo root:

```
target/jniLibs/                      # `--features cache` (the release-artifact default):
├── arm64-v8a/libpqc_client.so       # ~5.5 MiB  (modern devices)
├── armeabi-v7a/libpqc_client.so     # ~3.2 MiB  (old 32-bit ARM)
└── x86_64/libpqc_client.so          # ~6.7 MiB  (emulator; not shipped via Play)

generated/kotlin/
└── io/github/sriharsha_y/pqc/
    └── pqc.kt                       (UniFFI-generated Kotlin bindings)
```

The `.so` is dynamically linked, so the file size IS the on-device install
delta (the Play App Bundle ships only the device's ABI). With Play's default
on-the-wire compression the **download** size for arm64-v8a is ~2.6 MiB; the
**installed** footprint is ~5.5 MiB (the `.so` is stored uncompressed inside
the APK so `dlopen` can `mmap` it). Dropping the cache feature
(`PQC_CARGO_FEATURES=""` when building) saves ~0.8 MiB on arm64-v8a.

## 2. Packaging options

### A — Maven Central (recommended)

The library publishes to Maven Central on every release under the coordinates `io.github.sriharsha-y:pqc-mobile-client`. In the consumer's `build.gradle.kts`:

```kotlin
dependencies {
    implementation("io.github.sriharsha-y:pqc-mobile-client:0.10.1") // x-release-please-version
}
```

That pulls the AAR (with `arm64-v8a`, `armeabi-v7a`, `x86_64` slices), the Kotlin bindings, and the JNA + kotlinx-coroutines transitive dependencies in one declaration. No local cargo build, no manual `.so` vendoring.

> **If you use `PqcInterceptor`** (Section 3), declare OkHttp in your own dependencies — the AAR declares OkHttp as `compileOnly`, so it doesn't propagate transitively. Almost every Android consumer already has OkHttp (directly or via Retrofit / Ktor), but pure `HttpURLConnection` consumers who later add `PqcInterceptor` need to add:
> ```kotlin
> implementation("com.squareup.okhttp3:okhttp:4.9.0")  // or higher; 4.0+ supported
> ```

The published AAR is **self-contained**: it bundles the `rustls-platform-verifier` Kotlin glue (`org.rustls.platformverifier.*`) under its own `libs/` entry. Consumers do **not** need to add a separate Maven repository for it. Upstream ships those classes only as a vendored AAR inside the Cargo registry — `scripts/build-android.sh` extracts that jar and AGP embeds it into our AAR at publish time.

### B — Tarball from the GitHub Release (no Maven access)

For consumers behind corporate proxies that block Maven Central, or for early integration before Maven Central publication is ready, download `pqc-mobile-client-X.Y.Z-android.tar.gz` from the release page and unpack:

- `jniLibs/*` → `app/src/main/jniLibs/`
- `kotlin/io/github/sriharsha_y/pqc/*.kt` → `app/src/main/java/io/github/sriharsha_y/pqc/` (the generated `pqc.kt` plus the hand-written `PqcConfigDefaults.kt` and `PqcInterceptor.kt`)
- `libs/rustls-platform-verifier-*.jar` → `app/libs/`  (vendored Kotlin glue; without it the first TLS handshake throws `NoClassDefFoundError: org.rustls.platformverifier.CertificateVerifier`)

Add the JNA + coroutines deps to the consumer's `build.gradle` manually, plus `implementation(fileTree("libs") { include("*.jar") })` so AGP picks up the platform-verifier jar. If you use `PqcInterceptor`, add OkHttp too (see the note in Section 2A). Works but won't survive an Expo `prebuild`.

### C — Local Gradle module (development)

If you're hacking on `pqc-mobile-client` itself, build locally:

```bash
./scripts/build-android.sh
cd android && ./gradlew assembleRelease
```

The AAR lands at `android/build/outputs/aar/pqc-mobile-client-release.aar`. Reference it from the consumer with `implementation(files("path/to/pqc-mobile-client-release.aar"))` plus the JNA + coroutines deps. The wrapper pins Gradle 8.7 to match CI; the parent build's `preBuild` task fails with a helpful error if you forgot to run `scripts/build-android.sh` first (it extracts the rustls-platform-verifier jar into `android/libs/` that the AAR bundles).

## 3. Native Android — OkHttp / Retrofit / Ktor

OkHttp's `Interceptor` is the universal hook. The AAR ships an `open` base class `PqcInterceptor` whose defaults align with `OkHttpClient.Builder()` (10 s timeouts, no cache, 20-redirect cap) — with one deliberate divergence: cookies are managed by the Rust client (matching iOS), since OkHttp's own `cookieJar` is bypassed by the interceptor chain anyway. Subclass `PqcInterceptor` to customise the knobs you care about; the rest of the OkHttp pipeline — Retrofit, Ktor with the OkHttp engine, Apollo — inherits the swap unchanged.

```kotlin
import android.content.Context
import io.github.sriharsha_y.pqc.PqcConfig
import io.github.sriharsha_y.pqc.PqcInterceptor
import io.github.sriharsha_y.pqc.RedirectPolicy
import io.github.sriharsha_y.pqc.platformDefault

class AppPqcInterceptor(context: Context) : PqcInterceptor(context) {
    override fun makeConfig(context: Context): PqcConfig =
        PqcConfig.platformDefault(
            context = context,
            pinnedCertSha256 = CertPins.SPKI_SHA256,    // see §10
            defaultTimeoutMs = 15_000UL,
            userAgent = "MyApp/1.0",                    // identify to bank WAF / Akamai
            redirectPolicy = RedirectPolicy.SameOriginOnly,
        )
}

// Install. Call prewarm() at app start to surface PqcHttpClient
// construction failures (bad pin, missing native library) here instead of
// on the first user request — wrap in try/catch to fall back to default
// OkHttp on failure if you want graceful degradation.
val pqc = AppPqcInterceptor(context).also { it.prewarm() }
val okHttp = OkHttpClient.Builder()
    .addInterceptor(authHeaderInterceptor)        // runs first
    .addInterceptor(observabilityInterceptor)
    .addInterceptor(pqc)                          // MUST be last — no chain.proceed()
    .build()

// Retrofit / Ktor / Apollo sit on top of okHttp unchanged.
val retrofit = Retrofit.Builder()
    .baseUrl("https://api.example.com/")
    .client(okHttp)
    .addConverterFactory(MoshiConverterFactory.create())
    .build()
```

That is the entire interceptor — no body-drain, no header conversion, no `parseProtocol`/`statusReasonPhrase` to maintain. The base class also:

- strips any inbound `Cookie:` header so the Rust client's jar is the only source of truth — OkHttp's `BridgeInterceptor` (the cookie-injection layer) runs *after* application interceptors and is bypassed when the chain short-circuits, so without this you'd get inconsistent cookie state across consumers.
- wraps `PqcException` in `IOException` so callers can catch the standard OkHttp error type.
- defaults to `enableCookies = true` (Rust client manages cookies — matches iOS and keeps session-based flows working through OkHttp interceptors) and `enableCache = false` (matches OkHttp's `cache = null`). Flip `enableCache` on in `makeConfig` to enable the Rust RFC 9111 cache; set `enableCookies = false` if you explicitly do not want any cookie state.

## 4. Native Android — `HttpURLConnection` or no framework

`HttpURLConnection` does not expose an interceptor model. The clean answer is to skip it and call `PqcHttpClient` directly (see Section 6). If the consumer code must keep `HttpURLConnection` semantics, wrap `PqcHttpClient` behind a thin `HttpURLConnection`-shaped facade — possible but ~150 LOC of glue, not provided here.

## 5. React Native Android

The RN networking module reads its `OkHttpClient` from `OkHttpClientProvider`. The factory **must** be installed before `super.onCreate()` to avoid [RN issue #34789](https://github.com/facebook/react-native/issues/34789) where a late swap silently no-ops.

```kotlin
// android/app/src/main/java/com/yourapp/MainApplication.kt
import com.facebook.react.modules.network.OkHttpClientProvider
import com.facebook.react.modules.network.OkHttpClientFactory
import com.facebook.react.modules.network.ReactCookieJarContainer
import io.github.sriharsha_y.pqc.android.PqcAndroidInit
import java.util.concurrent.TimeUnit

class MainApplication : Application(), ReactApplication {
    override fun onCreate() {
        // Install BEFORE super.onCreate(), or NetworkingModule may already
        // have built its client (react-native#34789 — silent no-op).
        PqcAndroidInit.init(this)
        val pqcInterceptor = AppPqcInterceptor(applicationContext)
        OkHttpClientProvider.setOkHttpClientFactory(OkHttpClientFactory {
            OkHttpClient.Builder()
                .connectTimeout(0, TimeUnit.MILLISECONDS)
                .readTimeout(0, TimeUnit.MILLISECONDS)
                .writeTimeout(0, TimeUnit.MILLISECONDS)
                .cookieJar(ReactCookieJarContainer())
                .addInterceptor(authHeaderInterceptor)
                .addInterceptor(observabilityInterceptor)
                .addInterceptor(pqcInterceptor)             // MUST be last
                .build()
        })
        super.onCreate()
        SoLoader.init(this, false)
        // ... rest of RN init
    }
}
```

The `AppPqcInterceptor` class is the subclass defined in Section 3.

## 6. Direct use — no HTTP framework

For new code paths that don't have an existing HTTP client to swap, use `PqcHttpClient` directly. `PqcConfig.platformDefault(context, ...)` gives you OkHttp-aligned defaults so you only have to specify what's different.

```kotlin
import io.github.sriharsha_y.pqc.*
import kotlinx.coroutines.runBlocking

val pqc = PqcHttpClient(
    PqcConfig.platformDefault(
        context = applicationContext,
        pinnedCertSha256 = CertPins.SPKI_SHA256,
        userAgent = "MyApp/1.0",
        redirectPolicy = RedirectPolicy.SameOriginOnly,
    )
)

// Suspending call from a coroutine
suspend fun fetchBalance(): String {
    val resp = pqc.request(HttpRequest(
        method = HttpMethod.GET,
        url = "https://api.bank.example/accounts/123/balance",
        headers = mapOf("Authorization" to listOf("Bearer $token")),
        body = null,
        timeoutMs = null,
    ))
    // resp is `PqcResponse` — streaming-first like OkHttp ResponseBody.
    // `bytes()` is the buffered convenience matching `body.bytes()`;
    // for large downloads loop on `readChunk()` to keep heap bounded.
    return String(resp.bytes(), Charsets.UTF_8)
}

// Streaming large downloads to disk without buffering the whole body
suspend fun downloadLargeFile(url: String, dest: java.io.File) {
    val resp = pqc.request(HttpRequest(
        method = HttpMethod.GET,
        url = url,
        headers = emptyMap(),
        body = null,
        timeoutMs = null,
    ))
    if (resp.status() != 200.toUShort()) error("HTTP ${resp.status()}")
    dest.outputStream().use { out ->
        while (true) {
            val chunk = resp.readChunk() ?: break
            out.write(chunk)
        }
    }
}

// Cancellation — UniFFI 0.29 does NOT propagate coroutine cancellation
// into Rust. Call `resp.cancel()` explicitly when you want to abort a
// download mid-stream. Idempotent.
//
//   val resp = pqc.request(req)
//   try {
//       resp.bytes()
//   } finally {
//       resp.cancel()  // safe even after bytes(); idempotent
//   }
//
// Dropping the `PqcResponse` reference also aborts when GC reclaims it,
// but explicit `cancel()` releases the connection immediately.

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

Debug-build verification call. To confirm the negotiated key exchange, read the **server's** report from Cloudflare's `/cdn-cgi/trace` (the `kex=` line) — `PqcResponse` deliberately does not expose the group, because it is a per-connection property the client can only observe via a racy process-global:

```kotlin
val resp = pqc.request(HttpRequest(
    method = HttpMethod.GET,
    url = "https://pq.cloudflareresearch.com/cdn-cgi/trace",
    headers = emptyMap<String, List<String>>(), body = null, timeoutMs = 5000UL,
))
val kex = String(resp.bytes()).lineSequence()
    .firstOrNull { it.startsWith("kex=") }?.removePrefix("kex=")
android.util.Log.i("PQC", "kex=$kex alpn=${resp.negotiatedProtocol()}")
// kex == "X25519MLKEM768" → post-quantum; "X25519" → classical.
// `negotiatedProtocol()` is per-request ("h2", "http/1.1").
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

## 11. Response caching (opt-in)

The Rust core can cache HTTP responses (RFC 9111), so repeat GETs are served without a network round-trip — the same idea as an OkHttp `Cache`, but it lives in the core because the core owns the socket (OkHttp's own `Cache` is bypassed by the interceptor, see §5). It is **off by default**. Enable it per client:

```kotlin
import java.io.File

val httpCacheDir = File(context.cacheDir, "pqc-http").absolutePath
val config = PqcConfig(
    // … existing fields …
    redirectPolicy = RedirectPolicy.SameOriginOnly,
    enableCache = true,
    cacheDir = httpCacheDir,                // required on Android — no dir, no cache
    maxCacheBytes = 10uL * 1024u * 1024u,   // 10 MiB, like a typical OkHttp Cache; null → 20 MiB
)

// On logout / session end, drop everything (also good for a "Clear cache" button):
client.clearCache()
val bytes: ULong = client.cacheSizeBytes()  // e.g. to render "Clear cache (1.2 MB)"
```

**Use exactly one cache.** Leave OkHttp's `Cache` unset when this is enabled — the interceptor already bypasses it, so the core's cache is the single source of truth and there's no double storage.

### What gets cached

Cacheability is decided by method + status + cache headers — not by extension or `Content-Type`. This is a **private** cache (`shared = false`), so it will cache `Authorization`-bearing responses when their headers permit (same as OkHttp/`URLCache`). Use `Cache-Control: no-store` server-side to keep sensitive endpoints out; `clearCache()` on logout is the backstop.

### Notes / behavior vs. native

- **Builds:** only effective in artifacts built with the `cache` cargo feature (the official release builds enable it). In a feature-less build, `enableCache = true` makes the constructor throw `PqcError.InvalidRequest`, and `clearCache`/`cacheSizeBytes` are inert.
- **Eviction** is by insertion time (FIFO) once `maxCacheBytes` is exceeded — a close approximation of OkHttp's LRU (the disk store exposes no access time).
- **Diagnostic header:** every response that flows through the cache layer carries `x-pqc-cache-hit: true` (served from the mem or disk tier) or `x-pqc-cache-hit: false` (cache miss). Absent when the cache layer wasn't engaged (`enableCache = false`). Useful for verifying cache behaviour at runtime; consumers can ignore it.
- **Security:** a cache *hit* serves bytes without a TLS handshake, so the PQC / pinning guarantees re-apply only on a miss or revalidation. That's expected and matches every HTTP cache.

## 12. DNS resolver — `dnsResolver` (opt-in)

By default the client uses libc `getaddrinfo` driven on tokio's blocking pool. On Android this **honors the user-configured Private DNS (DNS-over-TLS) setting** in *Settings → Network & internet → Private DNS*. Most apps want this — leave `dnsResolver` unset.

Set `dnsResolver = DnsResolver.Hickory` to switch to the bundled `hickory-dns` async resolver. This enables **RFC 8305 Happy Eyeballs** — concurrent IPv4/IPv6 connection racing, materially faster on dual-stack networks where one address family is broken (common on some cellular carriers). The trade-off: **hickory bypasses the system Private DNS setting**, so consumers whose users depend on DoT for privacy or enterprise policy should leave the resolver at the default `System`.

```kotlin
val config = PqcConfig(
    // ...
    dnsResolver = DnsResolver.Hickory,  // opt-in for Happy Eyeballs
)
```

## 13. Streaming upload bodies — `BodyProvider` (large file uploads)

If you're using `PqcInterceptor` (§3), large uploads are handled automatically: OkHttp `RequestBody` instances with `contentLength() > 64 KiB` or unknown length route through an internal `BodyProvider` adapter that streams chunk-by-chunk via `okio.Pipe`. **Peak memory tracks one chunk (~64 KiB)**, not the file size — matches OkHttp's `RequestBody.writeTo()` semantics.

For consumers calling `PqcHttpClient` directly (Section 6), implement `BodyProvider` in Kotlin and set `HttpRequest.bodyStream`:

```kotlin
import io.github.sriharsha_y.pqc.BodyProvider
import io.github.sriharsha_y.pqc.HttpRequest
import io.github.sriharsha_y.pqc.HttpMethod
import io.github.sriharsha_y.pqc.PqcException
import java.io.InputStream

class StreamBodyProvider(private val stream: InputStream) : BodyProvider {
    private val buf = ByteArray(64 * 1024)
    @Volatile private var closed = false

    override fun nextChunk(): ByteArray? {
        if (closed) return null
        val n = try { stream.read(buf) }
                catch (t: Throwable) {
                    throw PqcException.InvalidRequest("read failed: ${t.message}")
                }
        if (n <= 0) { close(); return null }
        return buf.copyOf(n)
    }

    override fun cancel() {
        // Idempotent — Rust calls this on upload abort. Release the fd
        // immediately instead of waiting for the binding handle to drop.
        if (!closed) { closed = true; try { stream.close() } catch (_: Throwable) {} }
    }

    private fun close() = cancel()
}

val fileStream = file.inputStream()
val resp = runBlocking {
    pqc.request(HttpRequest(
        method = HttpMethod.POST,
        url = "https://api.example.com/upload",
        headers = mapOf("Content-Type" to listOf("application/octet-stream")),
        body = null,                                  // ← mutually exclusive
        bodyStream = StreamBodyProvider(fileStream),  // ← stream
        bodyStreamLength = file.length().toULong(),   // optional Content-Length;
                                                      //   null → chunked encoding
        timeoutMs = null,
    ))
}
```

`nextChunk()` is invoked from Rust via tokio `spawn_blocking`, so blocking reads (`InputStream.read`, file I/O) are safe. `cancel()` is called when the upload aborts (network error, caller dropped the request, server closed mid-stream) — implement it to release file descriptors and other resources. **Streaming bodies are not retry-safe** — once consumed, they can't be replayed; construct a fresh `BodyProvider` if you need to retry.

## 14. Tuning knobs

Beyond the basics in §3, `PqcConfig` exposes the following knobs (all optional, named args on `PqcConfig(...)` or `PqcConfig.platformDefault(...)`):

| Field | Default | Notes |
|---|---|---|
| `readIdleTimeoutMs` | `null` | Per-read idle timeout — kills a stalled stream without burning the total `defaultTimeoutMs` budget. Mirrors OkHttp's `readTimeout`. Recommended: 10–30 s for APIs, 60 s+ for large file downloads. |
| `maxInflightTotal` | `64U` | Global concurrent-request cap. `null` disables. Matches OkHttp `Dispatcher.maxRequests`. |
| `maxInflightPerHost` | `5U` | Per-host concurrent-request cap. `null` disables. Matches OkHttp `Dispatcher.maxRequestsPerHost`. |
| `maxMemoryCacheBytes` | `null` (= 4 MiB) | In-memory LRU tier for the response cache, **enabled by default on Android too**. Set to `0uL` for OkHttp-style disk-only behavior (OkHttp's bundled `Cache` is disk-only because its `Cache` class is `final`, not for a fundamental Android reason). |
| `dnsResolver` | `null` (= `System`) | See §12. |
| `proxyUrl` | `null` | Debug proxy — see §15. |

## 15. Debugging — routing through Charles / Proxyman / Burp (`proxyUrl`)

Because the Rust client runs its **own** TLS stack (rustls), it bypasses the platform networking layer — so web-debugging proxies don't observe it the way they observe `OkHttp`/`HttpURLConnection` traffic.

Set `proxyUrl` to route every request through your proxy so those tools can capture it:

```kotlin
val config = PqcConfig.platformDefault(
    context = appContext,
    pinnedCertSha256 = emptyList(),          // pinning OFF so the proxy CA is accepted
    proxyUrl = "http://192.168.1.5:8888",    // your machine's LAN IP + proxy port
)
```

Two prerequisites for HTTPS interception to actually work (identical to inspecting OkHttp):

1. **Trust the proxy CA.** On Android API 24+ apps don't trust user-installed CAs by default. Add a **debug** `network_security_config` that trusts user certs and reference it from a debug manifest:
   ```xml
   <!-- res/xml/network_security_config_debug.xml -->
   <network-security-config>
     <debug-overrides>
       <trust-anchors><certificates src="user"/></trust-anchors>
     </debug-overrides>
   </network-security-config>
   ```
   The Rust client's platform verifier delegates to the Android `TrustManager`, which honors this config.
2. **Leave pinning off** for the build you're debugging (empty `pinnedCertSha256`) — an active SPKI pin will (correctly) reject the proxy's MITM cert.

Notes:
- Use your machine's **LAN IP**, not `localhost`/`10.0.2.2` (the latter only for the emulator loopback). Embedded credentials are honored (`http://user:pass@host:port`); a bare `host:port` is treated as `http://`, and only an unparseable value fails `PqcHttpClient(config)` with `PqcError.InvalidRequest`.
- This is the supported way to inspect Rust-stack traffic; JS-XHR inspectors (Reactotron/Flipper) only see RN's JS layer and show incomplete URLs for these requests. **Leave `proxyUrl` `null` in production.**

## 16. Consuming from Java (not Kotlin)

The Kotlin-only sugar (`PqcConfig.platformDefault`, default-arg shortcuts, `suspend fun` on `PqcHttpClient.request`) interoperates with Java but requires a few adjustments:

- **`PqcConfig.platformDefault(...)`** is a top-level extension function on `PqcConfig.Companion`. From Java it lives at `PqcConfigDefaultsKt.platformDefault(context, …)`. Kotlin default-arg synthesis isn't exposed to Java, so **you must pass every parameter explicitly**; pass `null` for the optional `*_ms` / pin / UA / etc. parameters where you want the defaults to apply. Easier alternative: build a `PqcConfig` via its full constructor (UniFFI-generated; all fields visible from Java).
- **`PqcHttpClient.request(req)` is `suspend fun`**, which UniFFI 0.29 does **not** auto-bridge to Java (no `CompletableFuture` variant is generated). Write a small Kotlin facade and call it from Java:
   ```kotlin
   // PqcClientJavaBridge.kt
   import kotlinx.coroutines.GlobalScope
   import kotlinx.coroutines.future.future
   import java.util.concurrent.CompletableFuture

   object PqcClientJavaBridge {
       @JvmStatic
       fun requestAsync(client: PqcHttpClient, req: HttpRequest): CompletableFuture<PqcResponse> =
           GlobalScope.future { client.request(req) }
   }
   ```
   Add `implementation("org.jetbrains.kotlinx:kotlinx-coroutines-jdk8:1.7.x")` once. From Java: `PqcClientJavaBridge.requestAsync(client, req).get()` (blocking) or `.thenApply(...)` (callback). Calling the raw `suspend fun` from Java requires hand-constructing a `Continuation` — possible but error-prone; keep the suspension boundary on the Kotlin side.
- **`PqcException` is sealed** in Kotlin; Java sees it as a regular exception hierarchy (`PqcException.Network`, `PqcException.Tls`, `PqcException.PinningFailure`, `PqcException.TrustVerification`, `PqcException.Timeout`, `PqcException.InvalidRequest`). Catch the base or the specific subclass.
- **`PqcInterceptor` subclassing from Java** works — it's a Kotlin `open class`. Override `makeConfig(Context)` to return your `PqcConfig`. The interceptor is then added to OkHttp via `OkHttpClient.Builder.addInterceptor(pqcInterceptor)` the same way as in Kotlin.
- **`BodyProvider` is a Kotlin interface** with two methods (`nextChunk(): ByteArray?` and `cancel(): Unit`); Java implementations are straightforward — implement both.

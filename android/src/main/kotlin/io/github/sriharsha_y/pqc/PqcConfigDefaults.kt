package io.github.sriharsha_y.pqc

import android.content.Context
import android.os.Build

/**
 * Builds a [PqcConfig] whose defaults align with `OkHttpClient.Builder()`:
 * 10 s timeouts, no cache, up to 20 redirects. One deliberate divergence
 * — cookies are managed by the Rust client (matching iOS) since OkHttp's
 * `cookieJar` is bypassed by the chain order anyway. Safe to call from
 * any thread.
 *
 * The parameter list mirrors all 14 fields of [PqcConfig]. When a field
 * is added to the Rust struct, extend this signature — otherwise the new
 * field is only reachable via the full constructor or `.copy(...)`.
 *
 * Defaults for the optional concurrency/DNS/cache knobs match the
 * `#[uniffi(default = ...)]` annotations on the Rust struct so callers
 * who don't pass them get the exact same shape as the no-arg
 * `PqcConfig(...)` constructor.
 */
fun PqcConfig.Companion.platformDefault(
    context: Context? = null,
    pinnedCertSha256: List<String> = emptyList(),
    defaultTimeoutMs: ULong? = 10_000UL,
    connectTimeoutMs: ULong? = 10_000UL,
    readIdleTimeoutMs: ULong? = null,
    enableCookies: Boolean = true,
    userAgent: String? = null,
    dnsResolver: DnsResolver? = null,
    redirectPolicy: RedirectPolicy = RedirectPolicy.Limited(max = 20U),
    maxInflightTotal: UInt? = 64U,
    maxInflightPerHost: UInt? = 5U,
    enableCache: Boolean = false,
    cacheDir: String? = null,
    maxCacheBytes: ULong? = null,
    maxMemoryCacheBytes: ULong? = null,
): PqcConfig = PqcConfig(
    pinnedCertSha256 = pinnedCertSha256,
    defaultTimeoutMs = defaultTimeoutMs,
    connectTimeoutMs = connectTimeoutMs,
    readIdleTimeoutMs = readIdleTimeoutMs,
    enableCookies = enableCookies,
    userAgent = userAgent
        ?: context?.let { defaultAndroidUserAgent(it) }
        ?: "PqcCore (Android ${Build.VERSION.RELEASE})",
    dnsResolver = dnsResolver,
    redirectPolicy = redirectPolicy,
    maxInflightTotal = maxInflightTotal,
    maxInflightPerHost = maxInflightPerHost,
    enableCache = enableCache,
    cacheDir = cacheDir,
    maxCacheBytes = maxCacheBytes,
    maxMemoryCacheBytes = maxMemoryCacheBytes,
)

/**
 * Best-effort `"<applicationId>/<versionName> (Android <release>; <model>)"`.
 * reqwest's default UA gets flagged by many WAFs (Akamai, bank allowlists),
 * so we always send something recognisable when the caller passes null.
 */
fun defaultAndroidUserAgent(context: Context): String {
    val appCtx = context.applicationContext
    val pkg = appCtx.packageName
    val version = try {
        @Suppress("DEPRECATION")
        appCtx.packageManager.getPackageInfo(pkg, 0).versionName ?: "0"
    } catch (_: Exception) {
        "0"
    }
    return "$pkg/$version (Android ${Build.VERSION.RELEASE}; ${Build.MODEL})"
}

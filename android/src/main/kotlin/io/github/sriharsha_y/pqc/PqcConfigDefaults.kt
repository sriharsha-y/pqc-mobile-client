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
 * Mirrors all 15 fields of [PqcConfig]. The Rust-side drift detector in
 * `src/config.rs` compile-errors if a field is added without extending
 * this signature.
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
    proxyUrl: String? = null,
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
    proxyUrl = proxyUrl,
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

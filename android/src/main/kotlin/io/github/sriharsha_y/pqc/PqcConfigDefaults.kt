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
 * The parameter list mirrors the 9 fields of [PqcConfig]. When a field is
 * added to the Rust struct, this signature should be extended — otherwise
 * the new field silently picks up the binding's default value.
 */
fun PqcConfig.Companion.platformDefault(
    context: Context? = null,
    pinnedCertSha256: List<String> = emptyList(),
    defaultTimeoutMs: ULong? = 10_000UL,
    connectTimeoutMs: ULong? = 10_000UL,
    enableCookies: Boolean = true,
    userAgent: String? = null,
    redirectPolicy: RedirectPolicy = RedirectPolicy.Limited(max = 20U),
    enableCache: Boolean = false,
    cacheDir: String? = null,
    maxCacheBytes: ULong? = null,
): PqcConfig = PqcConfig(
    pinnedCertSha256 = pinnedCertSha256,
    defaultTimeoutMs = defaultTimeoutMs,
    connectTimeoutMs = connectTimeoutMs,
    enableCookies = enableCookies,
    userAgent = userAgent
        ?: context?.let { defaultAndroidUserAgent(it) }
        ?: "PqcCore (Android ${Build.VERSION.RELEASE})",
    redirectPolicy = redirectPolicy,
    enableCache = enableCache,
    cacheDir = cacheDir,
    maxCacheBytes = maxCacheBytes,
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

package io.github.sriharsha_y.pqc

import android.content.Context
import kotlinx.coroutines.runBlocking
import okhttp3.Headers
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import java.io.IOException

/**
 * OkHttp [Interceptor] that routes calls through the Rust [PqcHttpClient]
 * so the handshake uses rustls + rustls-post-quantum (X25519MLKEM768)
 * instead of OkHttp's TLS stack. Subclass and override [makeConfig],
 * [shouldIntercept], or [onSkip] to customise.
 *
 * **Must be added as the LAST application interceptor** — this class does
 * not call `chain.proceed()`, so any later interceptor never runs. OkHttp's
 * `BridgeInterceptor` (cookies, `Host`, gzip) and `CacheInterceptor` are
 * bypassed too; the Rust client owns cookies and cache instead.
 */
open class PqcInterceptor(context: Context) : Interceptor {

    private val appContext: Context = context.applicationContext

    /** Override to customise. Default: [PqcConfig.platformDefault]. */
    protected open fun makeConfig(context: Context): PqcConfig =
        PqcConfig.platformDefault(context)

    /** Whether this call should be routed through PQC. Default: HTTPS only. */
    protected open fun shouldIntercept(request: Request): Boolean =
        request.url.isHttps

    /** Called when [shouldIntercept] returns false. Default: `chain.proceed()`. */
    protected open fun onSkip(chain: Interceptor.Chain): Response =
        chain.proceed(chain.request())

    private val pqcClient: PqcHttpClient by lazy {
        PqcHttpClient(makeConfig(appContext))
    }

    /**
     * Force construction of the underlying [PqcHttpClient] so config errors
     * surface here instead of on the first user request. Recommended at app
     * start, wrapped in try/catch:
     *
     * ```
     * val pqc = try {
     *     AppPqcInterceptor(applicationContext).also { it.prewarm() }
     * } catch (t: Throwable) { Log.e(...); return }
     * ```
     */
    fun prewarm() {
        @Suppress("UnusedExpression")
        pqcClient
    }

    final override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()
        if (!shouldIntercept(req)) return onSkip(chain)

        // Strip Cookie: (Rust jar is authoritative — OkHttp's BridgeInterceptor
        // is bypassed by the chain order). Single-pass build of the multi-value
        // header map.
        val headers = LinkedHashMap<String, List<String>>(req.headers.size)
        for (i in 0 until req.headers.size) {
            val name = req.headers.name(i)
            if (name.equals("Cookie", ignoreCase = true)) continue
            val current = headers[name]
            headers[name] = if (current == null) listOf(req.headers.value(i))
                            else current + req.headers.value(i)
        }

        val bodyBytes: ByteArray? = req.body?.let { body ->
            val buf = Buffer()
            body.writeTo(buf)
            buf.readByteArray()
        }

        val pqcResp = try {
            runBlocking {
                pqcClient.request(
                    HttpRequest(
                        method = req.method.toPqcMethod(),
                        url = req.url.toString(),
                        headers = headers,
                        body = bodyBytes,
                        timeoutMs = null,
                    )
                )
            }
        } catch (e: Exception) {
            // Interceptors must throw IOException. Pass through unchanged so
            // callers can match on subtypes (SocketTimeoutException etc.);
            // wrap PqcException / NPE / JNA RuntimeException as the generic
            // IOException Retrofit/Apollo expect.
            throw if (e is IOException) e else IOException(e.message, e)
        }

        val mediaType = pqcResp.headers["content-type"]
            ?.firstOrNull()
            ?.toMediaTypeOrNull()

        // Post-redirect URL for response.request.url; fall back to the
        // original only if finalUrl is unparseable (it shouldn't be).
        val effectiveRequest = pqcResp.finalUrl.toHttpUrlOrNull()
            ?.let { req.newBuilder().url(it).build() }
            ?: req

        val responseBuilder = Response.Builder()
            .request(effectiveRequest)
            .protocol(parseProtocol(pqcResp.negotiatedProtocol))
            .code(pqcResp.status.toInt())
            .message(statusReasonPhrase(pqcResp.status.toInt()))
            .body(pqcResp.body.toResponseBody(mediaType))

        val headerBuilder = Headers.Builder()
        for ((name, values) in pqcResp.headers) {
            // Set-Cookie stays in the Rust jar; not surfaced to OkHttp's
            // cookieJar (which the chain order bypasses anyway).
            if (name.equals("Set-Cookie", ignoreCase = true)) continue
            // addUnsafeNonAscii skips OkHttp's RFC 7230 name/value
            // validation — these values came from the Rust client which
            // has already produced HTTP-clean output.
            for (v in values) headerBuilder.addUnsafeNonAscii(name, v)
        }
        responseBuilder.headers(headerBuilder.build())

        return responseBuilder.build()
    }

    // MARK: - Helpers

    private fun String.toPqcMethod(): HttpMethod = when (uppercase()) {
        "GET" -> HttpMethod.GET
        "POST" -> HttpMethod.POST
        "PUT" -> HttpMethod.PUT
        "DELETE" -> HttpMethod.DELETE
        "PATCH" -> HttpMethod.PATCH
        "HEAD" -> HttpMethod.HEAD
        "OPTIONS" -> HttpMethod.OPTIONS
        // An unrecognised verb must FAIL loudly, not silently become a GET
        // (that would drop the body and turn a write into a read).
        else -> throw IOException("unsupported HTTP method: $this")
    }

    /**
     * Map the Rust core's `negotiated_protocol` (ALPN id) to OkHttp's
     * [Protocol]. Defaults to HTTP/1.1 on unknown values rather than
     * fabricating HTTP/2 — wrong telemetry is worse than conservative.
     * OkHttp has no HTTP/3 enum, so h3 maps to the closest, HTTP/2.
     */
    private fun parseProtocol(raw: String): Protocol = when (raw) {
        "http/0.9", "http/1.0" -> Protocol.HTTP_1_0
        "http/1.1" -> Protocol.HTTP_1_1
        "h2" -> Protocol.HTTP_2
        "h3" -> Protocol.HTTP_2
        else -> Protocol.HTTP_1_1
    }

    /**
     * Best-effort reason phrase for the synthesised response. An empty
     * message makes HttpLoggingInterceptor print a malformed-looking
     * status line, so we supply RFC 9110 phrases for the common codes;
     * unusual codes fall back to empty rather than guessing.
     */
    private fun statusReasonPhrase(status: Int): String = when (status) {
        200 -> "OK"
        201 -> "Created"
        202 -> "Accepted"
        204 -> "No Content"
        301 -> "Moved Permanently"
        302 -> "Found"
        304 -> "Not Modified"
        400 -> "Bad Request"
        401 -> "Unauthorized"
        403 -> "Forbidden"
        404 -> "Not Found"
        409 -> "Conflict"
        422 -> "Unprocessable Entity"
        429 -> "Too Many Requests"
        500 -> "Internal Server Error"
        502 -> "Bad Gateway"
        503 -> "Service Unavailable"
        504 -> "Gateway Timeout"
        else -> ""
    }
}

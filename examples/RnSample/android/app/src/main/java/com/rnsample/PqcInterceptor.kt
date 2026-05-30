package com.rnsample

import okhttp3.Headers
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import kotlinx.coroutines.runBlocking
import io.github.sriharsha_y.pqc.HttpMethod
import io.github.sriharsha_y.pqc.HttpRequest
import io.github.sriharsha_y.pqc.PqcHttpClient

/**
 * OkHttp [Interceptor] that delegates the entire request to the Rust
 * [PqcHttpClient] so the handshake uses rustls + rustls-post-quantum
 * (X25519MLKEM768) instead of the JDK / Conscrypt TLS stack.
 *
 * Must be the **last** interceptor: it does not call chain.proceed() and
 * synthesizes the Response from the Rust call. Earlier interceptors (auth,
 * logging) still run; OkHttp's cache / pool / TLS are bypassed because the
 * Rust core owns the socket.
 *
 * The opt-in [PQC_MODE_HEADER] = "off" makes the interceptor fall through to
 * OkHttp's own stack (Conscrypt) instead of the Rust client, so the sample can
 * contrast the PQC handshake with the platform's classical one. Production
 * needs neither the header nor the fall-through — just route through the client.
 */
class PqcInterceptor(
    private val pqcClient: PqcHttpClient,
) : Interceptor {

    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()

        // Toggle "off": don't route through the Rust client — proceed with
        // OkHttp's own stack (Conscrypt) so the sample can contrast PQC vs.
        // the platform handshake. Strip the marker header so it never leaves.
        if (req.header(PQC_MODE_HEADER)?.equals("off", ignoreCase = true) == true) {
            return chain.proceed(req.newBuilder().removeHeader(PQC_MODE_HEADER).build())
        }

        val bodyBytes: ByteArray? = req.body?.let {
            val buf = Buffer()
            it.writeTo(buf)
            buf.readByteArray()
        }

        val headers = req.headers.toMultimap().toMutableMap()

        val pqcResp = runBlocking {
            pqcClient.request(
                HttpRequest(
                    method = req.method.toPqcMethod(),
                    url = req.url.toString(),
                    // toMultimap() preserves duplicate header values
                    // (Kotlin's Iterable.toMap() would drop all but the
                    // last entry for any repeated header name).
                    headers = headers,
                    body = bodyBytes,
                    timeoutMs = null,
                )
            )
        }

        val mediaType = pqcResp.headers["content-type"]
            ?.firstOrNull()
            ?.toMediaTypeOrNull()

        // Authoritative URL for OkHttp's CookieJar / response.request.url:
        // the post-redirect URL the body actually came from. Fall back to the
        // original request only if the Rust core's finalUrl is unparseable
        // (it should not be).
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
            for (v in values) headerBuilder.add(name, v)
        }
        responseBuilder.headers(headerBuilder.build())

        return responseBuilder.build()
    }

    companion object {
        /** Opt-in request header the RN sample sets to select the
         * classical-only client ("off"). Stripped before the request
         * leaves the device. */
        const val PQC_MODE_HEADER = "X-Pqc-Mode"
    }

    private fun String.toPqcMethod(): HttpMethod = when (uppercase()) {
        "GET" -> HttpMethod.GET
        "POST" -> HttpMethod.POST
        "PUT" -> HttpMethod.PUT
        "DELETE" -> HttpMethod.DELETE
        "PATCH" -> HttpMethod.PATCH
        "HEAD" -> HttpMethod.HEAD
        "OPTIONS" -> HttpMethod.OPTIONS
        else -> error("unsupported HTTP method: $this")
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
     * Best-effort reason phrase for the synthesized response. An empty
     * message makes HttpLoggingInterceptor print a malformed-looking status
     * line, so we supply RFC 9110 phrases for the common codes; unusual
     * codes fall back to empty rather than guessing.
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

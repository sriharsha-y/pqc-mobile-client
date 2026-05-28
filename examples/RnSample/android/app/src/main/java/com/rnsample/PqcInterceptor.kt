package com.rnsample

import okhttp3.Headers
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
 * Takes TWO clients because `enable_post_quantum` is fixed at construction:
 * a PQC client and a classical-only one, picked per request via the opt-in
 * [PQC_MODE_HEADER]. Production needs only the PQC client — this duality
 * exists purely to demonstrate both paths.
 */
class PqcInterceptor(
    private val pqcClient: PqcHttpClient,
    private val classicalClient: PqcHttpClient,
) : Interceptor {

    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()

        val bodyBytes: ByteArray? = req.body?.let {
            val buf = Buffer()
            it.writeTo(buf)
            buf.readByteArray()
        }

        // Route on the opt-in mode header, then strip it so it never
        // leaves the device. "off" → classical-only client.
        val classicalOnly = req.header(PQC_MODE_HEADER)?.equals("off", ignoreCase = true) == true
        val client = if (classicalOnly) classicalClient else pqcClient

        val headers = req.headers.toMultimap().toMutableMap().apply {
            // Case-insensitive removal: toMultimap() lowercases names.
            remove(PQC_MODE_HEADER.lowercase())
        }

        val pqcResp = runBlocking {
            client.request(
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

        val responseBuilder = Response.Builder()
            .request(req)
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

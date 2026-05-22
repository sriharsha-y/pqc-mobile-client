package com.rnsample

import okhttp3.Headers
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import kotlinx.coroutines.runBlocking
import uniffi.pqc.HttpMethod
import uniffi.pqc.HttpRequest
import uniffi.pqc.PqcHttpClient

/**
 * OkHttp [Interceptor] that delegates the entire request to the Rust
 * [PqcHttpClient] so the handshake uses rustls + rustls-post-quantum
 * (X25519MLKEM768) instead of the JDK / Conscrypt TLS stack.
 *
 * Must be added as the **last** interceptor on the OkHttpClient — it
 * does not chain.proceed() and instead manufactures the Response from
 * the Rust call.
 *
 * Other interceptors registered BEFORE this one (auth, logging,
 * tracing) still run as normal. OkHttp's cache / connection pool /
 * TLS config are bypassed because the Rust core owns the socket.
 */
class PqcInterceptor(private val client: PqcHttpClient) : Interceptor {

    override fun intercept(chain: Interceptor.Chain): Response {
        val req = chain.request()

        val bodyBytes: ByteArray? = req.body?.let {
            val buf = Buffer()
            it.writeTo(buf)
            buf.readByteArray()
        }

        val pqcResp = runBlocking {
            client.request(
                HttpRequest(
                    method = req.method.toPqcMethod(),
                    url = req.url.toString(),
                    headers = req.headers.toMap(),
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
        // Stamp the negotiated KEX as a synthetic header so JS / native code
        // can verify the handshake without reaching back into the Rust client.
        headerBuilder.add("X-Pqc-Negotiated-Group", pqcResp.negotiatedNamedGroup)
        responseBuilder.headers(headerBuilder.build())

        return responseBuilder.build()
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
     * Map the Rust core's `negotiated_protocol` string (formatted from
     * `http::Version` via `Debug`, e.g. "HTTP/1.1" or "HTTP/2.0") to
     * OkHttp's [Protocol] enum. Defaults to HTTP/1.1 on unknown values
     * rather than fabricating HTTP/2 — wrong telemetry is worse than
     * conservative telemetry. OkHttp lacks an HTTP/3 enum, so HTTP/3
     * is reported as HTTP/2 (the closest OkHttp can express).
     */
    private fun parseProtocol(raw: String): Protocol = when (raw) {
        "HTTP/0.9" -> Protocol.HTTP_1_0
        "HTTP/1.0" -> Protocol.HTTP_1_0
        "HTTP/1.1" -> Protocol.HTTP_1_1
        "HTTP/2.0", "HTTP/2" -> Protocol.HTTP_2
        "HTTP/3.0", "HTTP/3" -> Protocol.HTTP_2
        else -> Protocol.HTTP_1_1
    }

    /**
     * Best-effort reason phrase for the synthesized response. OkHttp's
     * Response.Builder.message() accepts any string, but logging
     * interceptors and downstream parsers (HttpLoggingInterceptor in
     * particular) print a malformed-looking status line when it's empty.
     * Standard RFC 9110 phrases for the codes a banking API actually
     * returns; unusual codes get the empty-string fallback rather than
     * pretending we know what 451-Unavailable-For-Legal-Reasons says.
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

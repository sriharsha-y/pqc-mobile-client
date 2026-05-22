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
            .protocol(Protocol.HTTP_2)
            .code(pqcResp.status.toInt())
            .message("")
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
}

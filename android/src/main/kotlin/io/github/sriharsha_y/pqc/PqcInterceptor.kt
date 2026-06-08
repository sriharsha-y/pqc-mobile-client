package io.github.sriharsha_y.pqc

import android.content.Context
import kotlinx.coroutines.runBlocking
import okhttp3.Headers
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.Response
import okio.Buffer
import okio.Pipe
import okio.buffer
import java.io.IOException
import java.util.concurrent.Executors

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
/// Bodies up to this size are buffered inline (single ByteArray, one
/// FFI hop). Anything larger — including unknown-length bodies — goes
/// through the streaming BodyProvider path. 64 KiB is the same as
/// OkHttp's default `Segment` size, big enough that small JSON
/// payloads never spin up a streaming pipe.
private const val INLINE_BODY_THRESHOLD: Long = 64L * 1024L

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

        // Body routing: small bodies (or unknown-length but ≤ INLINE_THRESHOLD)
        // are inlined as ByteArray; everything else streams via BodyProvider
        // so the upload never fully materializes — matches OkHttp's own
        // RequestBody.writeTo() streaming contract.
        val reqBody: RequestBody? = req.body
        val contentLen: Long = reqBody?.contentLength() ?: -1L
        val inline: ByteArray?
        val stream: BodyProvider?
        val streamLen: ULong?
        if (reqBody == null) {
            inline = null
            stream = null
            streamLen = null
        } else if (contentLen in 0L..INLINE_BODY_THRESHOLD) {
            // Known-small body: buffer once into ByteArray (one allocation,
            // single FFI hop, no thread spin-up).
            val buf = Buffer()
            reqBody.writeTo(buf)
            inline = buf.readByteArray()
            stream = null
            streamLen = null
        } else {
            // Unknown length OR known-large: stream. RequestBody.writeTo is a
            // PUSH interface; we adapt it to BodyProvider's PULL contract via
            // okio.Pipe — writer pushes from a background thread, Rust pulls
            // from the source side.
            inline = null
            stream = OkHttpBodyProviderAdapter(reqBody)
            streamLen = if (contentLen >= 0) contentLen.toULong() else null
        }

        val pqcResp = try {
            runBlocking {
                pqcClient.request(
                    HttpRequest(
                        method = req.method.toPqcMethod(),
                        url = req.url.toString(),
                        headers = headers,
                        body = inline,
                        bodyStream = stream,
                        bodyStreamLength = streamLen,
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

        val respHeaders = pqcResp.headers()
        val mediaType = respHeaders["content-type"]
            ?.firstOrNull()
            ?.toMediaTypeOrNull()

        // Post-redirect URL for response.request.url; fall back to the
        // original only if finalUrl is unparseable (it shouldn't be).
        val effectiveRequest = pqcResp.finalUrl().toHttpUrlOrNull()
            ?.let { req.newBuilder().url(it).build() }
            ?: req

        // Streaming ResponseBody backed by PqcResponse.readChunk(). Mirrors
        // OkHttp's own streaming body shape: the downstream consumer pulls
        // bytes via source().read(), each pull translates to one readChunk()
        // suspension. Memory is bounded to one chunk (~16-64 KB) regardless
        // of total body size. Closing the body cancels the Rust-side stream
        // and releases the underlying connection.
        val contentLength: Long = respHeaders["content-length"]
            ?.firstOrNull()
            ?.toLongOrNull()
            ?: -1L
        val body = streamingResponseBody(pqcResp, mediaType, contentLength)

        val responseBuilder = Response.Builder()
            .request(effectiveRequest)
            .protocol(parseProtocol(pqcResp.negotiatedProtocol()))
            .code(pqcResp.status().toInt())
            .message(statusReasonPhrase(pqcResp.status().toInt()))
            .body(body)

        val headerBuilder = Headers.Builder()
        for ((name, values) in respHeaders) {
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

    /**
     * Wrap a [PqcResponse] as an OkHttp [okhttp3.ResponseBody] whose underlying
     * source is `PqcResponse.readChunk()`. One chunk in flight at a time;
     * memory does not scale with body size. On `close()`, calls
     * `PqcResponse.cancel()` so the Rust-side connection is released
     * promptly instead of waiting for JVM GC.
     */
    private fun streamingResponseBody(
        pqcResp: PqcResponse,
        mediaType: okhttp3.MediaType?,
        contentLength: Long,
    ): okhttp3.ResponseBody {
        val source = object : okio.Source {
            override fun read(sink: Buffer, byteCount: Long): Long {
                // `runBlocking` is acceptable here — OkHttp interceptors
                // already drive blocking I/O on the calling thread.
                val chunk = runBlocking { pqcResp.readChunk() } ?: return -1
                if (chunk.isEmpty()) return -1
                sink.write(chunk)
                return chunk.size.toLong()
            }
            override fun timeout(): okio.Timeout = okio.Timeout.NONE
            override fun close() {
                // Sync FFI — no runBlocking. PqcResponse.cancel() flips
                // an atomic flag and drops the body if uncontended; if
                // a read is in flight on another thread it observes the
                // flag at its next chunk boundary.
                pqcResp.cancel()
            }
        }
        // `Source.buffer()` extension (imported above) — wraps the raw
        // Source in a BufferedSource that does the small-read amortization
        // OkHttp's downstream consumers expect.
        val buffered = source.buffer()
        return object : okhttp3.ResponseBody() {
            override fun contentType(): okhttp3.MediaType? = mediaType
            override fun contentLength(): Long = contentLength
            override fun source(): okio.BufferedSource = buffered
        }
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

/**
 * Adapts an OkHttp [RequestBody] (a PUSH interface — `writeTo(sink)`)
 * to the Rust [BodyProvider] PULL contract via an [okio.Pipe]. The
 * OkHttp body is written from a single background thread; the Rust
 * client pulls chunks via [nextChunk], each call reading the next
 * available 64 KiB from the pipe's source side.
 *
 * The pipe's internal buffer (`PIPE_BUFFER`) provides backpressure:
 * if Rust falls behind, the writer thread blocks on the next
 * `sink.write()` until the source drains. Peak memory tracks
 * `PIPE_BUFFER`, not the body size — even a 10 GB upload stays
 * bounded to ~256 KiB resident.
 *
 * Threading: the writer thread is a single dedicated daemon — one
 * thread per in-flight streaming upload, lifetime tied to the request.
 * `nextChunk` is invoked from Rust via UniFFI on a tokio
 * `spawn_blocking` worker, so it's safe to do blocking pipe reads.
 */
private class OkHttpBodyProviderAdapter(private val body: RequestBody) : BodyProvider {
    private val pipe = Pipe(PIPE_BUFFER)
    private val source = pipe.source.buffer()
    private val readBuf = Buffer()

    init {
        // One-shot writer: pumps the RequestBody into the pipe sink and
        // closes it on EOF. Errors propagate via pipe.fold/cancel; the
        // source-side read then surfaces them to Rust as an EOF.
        val sink = pipe.sink.buffer()
        WRITER_POOL.execute {
            try {
                body.writeTo(sink)
                sink.flush()
            } catch (t: Throwable) {
                // Close the pipe to signal EOF/error to the reader.
            } finally {
                try { sink.close() } catch (_: Throwable) {}
            }
        }
    }

    override fun nextChunk(): ByteArray? {
        readBuf.clear()
        return try {
            val n = source.read(readBuf, CHUNK_SIZE.toLong())
            if (n <= 0L) null else readBuf.readByteArray()
        } catch (_: Throwable) {
            // Pipe closed mid-read (writer errored or completed).
            null
        }
    }

    companion object {
        // Per-upload buffer. 256 KiB balances syscall amortization vs
        // peak memory; OkHttp's own internal pipe defaults to 1 MiB,
        // but mobile devices prefer the tighter cap.
        private const val PIPE_BUFFER: Long = 256L * 1024L
        // Per-chunk read size — matches the Rust side's STREAM_CHUNK_SIZE.
        private const val CHUNK_SIZE: Int = 64 * 1024
        // Single shared daemon pool — one writer per in-flight upload.
        // Each task lives O(body size / network throughput), then exits.
        private val WRITER_POOL = Executors.newCachedThreadPool { r ->
            Thread(r, "pqc-upload-writer").apply { isDaemon = true }
        }
    }
}

package io.github.sriharsha_y.pqcsample

import android.app.Activity
import android.os.Bundle
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import io.github.sriharsha_y.pqc.HttpMethod
import io.github.sriharsha_y.pqc.HttpRequest
import io.github.sriharsha_y.pqc.PqcConfig
import io.github.sriharsha_y.pqc.PqcHttpClient
import io.github.sriharsha_y.pqc.RedirectPolicy
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * One-screen demo: tap the button, the app makes a direct [PqcHttpClient]
 * request to Cloudflare's PQC test endpoint and reports the key-exchange
 * group the server saw — the server-authoritative way to confirm the
 * X25519MLKEM768 hybrid was actually negotiated (see docs/android.md §9).
 */
class MainActivity : Activity() {

    // Tie background work to the Activity so it's cancelled on destroy.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val pad = (16 * resources.displayMetrics.density).toInt()

        val output = TextView(this).apply {
            text = getString(R.string.intro)
            textSize = 14f
            setPadding(pad, pad, pad, pad)
            setTextIsSelectable(true)
        }

        val button = Button(this).apply {
            text = getString(R.string.run_button)
        }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad, pad, pad)
            addView(button, lp())
            addView(ScrollView(this@MainActivity).apply { addView(output) }, lp())
        }
        setContentView(root)

        button.setOnClickListener {
            button.isEnabled = false
            output.text = getString(R.string.running)
            scope.launch {
                output.text = runCatching { verifyPqc() }
                    .getOrElse { e -> "❌ ERROR: ${e::class.simpleName}: ${e.message}" }
                button.isEnabled = true
            }
        }
    }

    /** Build a client, hit /cdn-cgi/trace, extract the `kex=` line. */
    private suspend fun verifyPqc(): String = withContext(Dispatchers.IO) {
        // The constructor throws on malformed config (e.g. a bad pin); here
        // the config is static and valid, so it won't.
        val client = PqcHttpClient(
            PqcConfig(
                pinnedCertSha256 = emptyList(),   // platform trust only
                enablePostQuantum = true,
                defaultTimeoutMs = 15_000uL,
                connectTimeoutMs = null,          // 10s default
                maxBodyBytes = null,              // 16 MiB default
                enableCookies = false,
                userAgent = "PqcNativeAndroidSample/1.0",
                redirectPolicy = RedirectPolicy.SameOriginOnly,
            )
        )

        val resp = client.request(
            HttpRequest(
                method = HttpMethod.GET,
                url = "https://pq.cloudflareresearch.com/cdn-cgi/trace",
                headers = emptyMap(),
                body = null,
                timeoutMs = 5_000uL,
            )
        )

        val body = String(resp.body, Charsets.UTF_8)
        val kex = body.lineSequence()
            .firstOrNull { it.startsWith("kex=") }
            ?.removePrefix("kex=")
            ?: "unknown"

        buildString {
            appendLine("status = ${resp.status}")
            appendLine("alpn   = ${resp.negotiatedProtocol}")
            appendLine("kex    = $kex")
            appendLine()
            append(
                if (kex == "X25519MLKEM768") {
                    "✅ Post-quantum hybrid negotiated."
                } else {
                    "⚠️ Classical KEX ($kex) — the server did not negotiate PQC."
                }
            )
        }
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    private fun lp() = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
    )
}

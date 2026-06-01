package io.github.sriharsha_y.pqcsample

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.content.Context
import io.github.sriharsha_y.pqc.PqcConfig
import io.github.sriharsha_y.pqc.PqcInterceptor
import io.github.sriharsha_y.pqc.RedirectPolicy
import io.github.sriharsha_y.pqc.platformDefault
import okhttp3.OkHttpClient
import okhttp3.Request
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.net.URL
import javax.net.ssl.HttpsURLConnection

private const val MP = ViewGroup.LayoutParams.MATCH_PARENT
private const val WC = ViewGroup.LayoutParams.WRAP_CONTENT

/**
 * Dark card UI matching the React Native and SwiftUI samples: a toggle that
 * switches between this library's PQC client and the Android system stack
 * (HttpsURLConnection), and a result card showing the key-exchange group the
 * server saw (server-authoritative, via /cdn-cgi/trace). Auto-runs on launch
 * and on every toggle flip.
 */
class MainActivity : Activity() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    private lateinit var toggle: Switch
    private lateinit var toggleCaption: TextView
    private lateinit var resultContainer: LinearLayout

    // RN sample palette so the three samples look like one product.
    private val cBg = Color.parseColor("#0B0D11")
    private val cCard = Color.parseColor("#161A22")
    private val cTitle = Color.parseColor("#E7EAF0")
    private val cAccent = Color.parseColor("#5D97F7")
    private val cMuted = Color.parseColor("#7D8595")
    private val cKexPqc = Color.parseColor("#5DD193")
    private val cKexClass = Color.parseColor("#E8B94C")
    private val cError = Color.parseColor("#FF6F6F")

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(8), dp(16), dp(16))
        }

        column.addView(text("pqc-mobile-client", 22f, cTitle, bold = true))
        column.addView(
            text("Platform: Android API ${Build.VERSION.SDK_INT}", 13f, cAccent, bottom = dp(12))
        )
        column.addView(buildToggleCard())
        column.addView(buildResultCard())

        val root = ScrollView(this).apply {
            setBackgroundColor(cBg)
            addView(column, ViewGroup.LayoutParams(MP, MP))
        }
        setContentView(root)
        // targetSdk 35 draws edge-to-edge on Android 15+, so pad the root by
        // the status-bar / cutout / nav-bar insets to keep content clear of them.
        applySystemBarInsets(root)

        // Auto-run on launch (toggle defaults to ON / post-quantum).
        run(toggle.isChecked)
    }

    // ---- Toggle card -------------------------------------------------------

    private fun buildToggleCard(): View {
        val labels = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, WC, 1f)
        }
        labels.addView(text("Networking stack", 16f, cTitle, bold = true))
        toggleCaption = text("PQC client (this library)", 12f, cMuted)
        labels.addView(toggleCaption)

        toggle = Switch(this).apply {
            isChecked = true
            setOnCheckedChangeListener { _, checked ->
                toggleCaption.text =
                    if (checked) "PQC client (this library)"
                    else "System HttpURLConnection (no PQC)"
                run(checked)
            }
        }

        return card(horizontal = true).apply {
            gravity = Gravity.CENTER_VERTICAL
            addView(labels)
            addView(toggle)
        }
    }

    // ---- Result card -------------------------------------------------------

    private fun buildResultCard(): View {
        val card = card(horizontal = false)
        card.addView(text("Cloudflare /cdn-cgi/trace", 16f, cTitle, bold = true))
        card.addView(
            text("https://pq.cloudflareresearch.com/cdn-cgi/trace", 12f, cMuted, bottom = dp(4))
        )
        resultContainer = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        card.addView(resultContainer)
        return card
    }

    private fun showLoading() {
        resultContainer.removeAllViews()
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dp(8), 0, 0)
        }
        row.addView(ProgressBar(this).apply {
            layoutParams = LinearLayout.LayoutParams(dp(20), dp(20)).also { it.rightMargin = dp(10) }
        })
        row.addView(text("Performing TLS handshake…", 13f, cMuted))
        resultContainer.addView(row)
    }

    private fun showResult(status: UShort, kex: String?, alpn: String) {
        resultContainer.removeAllViews()
        resultContainer.addView(fieldLabel("Negotiated KEX (server-reported)"))
        if (kex != null) {
            val pqc = kex.uppercase().contains("MLKEM")
            resultContainer.addView(
                text(
                    kex + if (pqc) "  ✓ post-quantum" else "  (classical)",
                    16f, if (pqc) cKexPqc else cKexClass, mono = true,
                )
            )
            resultContainer.addView(
                caption(
                    if (pqc) "This library offered X25519MLKEM768; the edge negotiated it."
                    else "Android system HttpURLConnection — classical handshake; no PQC."
                )
            )
        } else {
            resultContainer.addView(text("not reported", 16f, cMuted, mono = true))
        }
        resultContainer.addView(fieldLabel("ALPN"))
        resultContainer.addView(text(alpn, 16f, cTitle, mono = true))
        resultContainer.addView(fieldLabel("HTTP status"))
        resultContainer.addView(text(status.toString(), 16f, cTitle, mono = true))
    }

    private fun showError(message: String) {
        resultContainer.removeAllViews()
        resultContainer.addView(fieldLabel("Error"))
        resultContainer.addView(text(message, 13f, cError))
    }

    // ---- The actual PQC request -------------------------------------------

    private fun run(usePqc: Boolean) {
        toggle.isEnabled = false
        showLoading()
        scope.launch {
            try {
                val (status, kex, alpn) =
                    if (usePqc) fetchViaPqcClient() else fetchViaSystemStack()
                showResult(status, kex, alpn)
            } catch (e: Exception) {
                showError("${e::class.simpleName}: ${e.message}")
            } finally {
                toggle.isEnabled = true
            }
        }
    }

    /** This library: routes the call through OkHttp + `PqcInterceptor`,
     *  which is the realistic Android integration pattern (Retrofit, Ktor,
     *  RN all sit on OkHttp). The interceptor swaps OkHttp's TLS for the
     *  Rust PQC client, so a PQ-capable edge reports kex=X25519MLKEM768. */
    private suspend fun fetchViaPqcClient(): Triple<UShort, String?, String> =
        withContext(Dispatchers.IO) {
            // Subclass `PqcInterceptor` to override `makeConfig` with the
            // sample's banking-style knobs on top of `platformDefault`.
            val pqcInterceptor = object : PqcInterceptor(applicationContext) {
                override fun makeConfig(context: Context): PqcConfig =
                    PqcConfig.platformDefault(
                        context = context,
                        defaultTimeoutMs = 15_000uL,
                        userAgent = "PqcNativeAndroidSample/1.0",
                        redirectPolicy = RedirectPolicy.SameOriginOnly,
                    )
            }
            val http = OkHttpClient.Builder()
                .addInterceptor(pqcInterceptor)    // MUST be last
                .build()
            val request = Request.Builder()
                .url("https://pq.cloudflareresearch.com/cdn-cgi/trace")
                .build()
            http.newCall(request).execute().use { resp ->
                val body = resp.body?.string() ?: ""
                val kex = body.lineSequence()
                    .firstOrNull { it.startsWith("kex=") }
                    ?.removePrefix("kex=")
                Triple(resp.code.toUShort(), kex, resp.protocol.toString())
            }
        }

    /** The Android system stack (HttpsURLConnection over Conscrypt). It does
     *  not offer the hybrid to apps, so the same edge reports kex=X25519 — the
     *  contrast this sample shows. The KEX is read from the server trace body,
     *  so it's authoritative regardless of which client made the request. */
    private suspend fun fetchViaSystemStack(): Triple<UShort, String?, String> =
        withContext(Dispatchers.IO) {
            val conn = (URL("https://pq.cloudflareresearch.com/cdn-cgi/trace")
                .openConnection() as HttpsURLConnection).apply {
                requestMethod = "GET"
                connectTimeout = 15_000
                readTimeout = 15_000
            }
            try {
                val status = conn.responseCode.toUShort()
                val body = conn.inputStream.bufferedReader().use { it.readText() }
                // HttpsURLConnection exposes no negotiated KEX/ALPN to the app.
                val kex = body.lineSequence()
                    .firstOrNull { it.startsWith("kex=") }
                    ?.removePrefix("kex=")
                Triple(status, kex, "n/a (system)")
            } finally {
                conn.disconnect()
            }
        }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    // ---- View helpers ------------------------------------------------------

    private fun card(horizontal: Boolean): LinearLayout {
        val bg = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            cornerRadius = dp(14).toFloat()
            setColor(cCard)
        }
        return LinearLayout(this).apply {
            orientation = if (horizontal) LinearLayout.HORIZONTAL else LinearLayout.VERTICAL
            background = bg
            setPadding(dp(20), dp(16), dp(20), dp(16))
            layoutParams = lp(bottom = dp(12))
        }
    }

    private fun fieldLabel(s: String) = text(s.uppercase(), 12f, cMuted, top = dp(12))

    private fun caption(s: String) = text(s, 12f, cMuted, top = dp(4)).apply {
        setTypeface(typeface, Typeface.ITALIC)
    }

    private fun text(
        s: String,
        sizeSp: Float,
        color: Int,
        bold: Boolean = false,
        mono: Boolean = false,
        top: Int = 0,
        bottom: Int = 0,
    ) = TextView(this).apply {
        text = s
        textSize = sizeSp
        setTextColor(color)
        if (bold) setTypeface(typeface, Typeface.BOLD)
        if (mono) typeface = Typeface.MONOSPACE
        setTextIsSelectable(true)
        layoutParams = lp(top = top, bottom = bottom)
    }

    private fun lp(top: Int = 0, bottom: Int = 0) =
        LinearLayout.LayoutParams(MP, WC).apply {
            topMargin = top
            bottomMargin = bottom
        }

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    /** Pad [view] by the system-bar + display-cutout insets (edge-to-edge). */
    private fun applySystemBarInsets(view: View) {
        view.setOnApplyWindowInsetsListener { v, insets ->
            val (top, bottom) = if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(
                    WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout()
                )
                bars.top to bars.bottom
            } else {
                @Suppress("DEPRECATION")
                (insets.systemWindowInsetTop to insets.systemWindowInsetBottom)
            }
            v.setPadding(v.paddingLeft, top, v.paddingRight, bottom)
            insets
        }
        view.requestApplyInsets()
    }
}

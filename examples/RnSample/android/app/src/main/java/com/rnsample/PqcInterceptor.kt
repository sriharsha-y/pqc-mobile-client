package com.rnsample

import android.content.Context
import io.github.sriharsha_y.pqc.PqcConfig
import io.github.sriharsha_y.pqc.PqcInterceptor as BasePqcInterceptor
import io.github.sriharsha_y.pqc.RedirectPolicy
import io.github.sriharsha_y.pqc.platformDefault
import okhttp3.Interceptor
import okhttp3.Request
import okhttp3.Response

/**
 * RN sample's [BasePqcInterceptor] subclass. The base class handles the
 * full request/response plumbing; here we only customise the [PqcConfig]
 * (banking-style posture) and add the `X-Pqc-Mode: off` toggle the sample
 * UI uses to contrast PQC vs OkHttp's own TLS stack.
 */
class RnSamplePqcInterceptor(
    context: Context,
) : BasePqcInterceptor(context) {

    override fun makeConfig(context: Context): PqcConfig =
        // Banking-style overrides on top of OkHttp defaults:
        //   - SameOriginOnly redirects (refuse cross-origin downgrades),
        //   - 15 s total budget,
        //   - explicit UA for Akamai / bank WAFs.
        // A real banking app MUST populate pinnedCertSha256 too.
        PqcConfig.platformDefault(
            context = context,
            defaultTimeoutMs = 15_000UL,
            userAgent = "RnSample/0.3.1 (pqc-mobile-client)",
            redirectPolicy = RedirectPolicy.SameOriginOnly,
        )

    override fun shouldIntercept(request: Request): Boolean {
        if (!super.shouldIntercept(request)) return false
        // X-Pqc-Mode: off → fall through to OkHttp's own stack so the
        // sample can contrast the PQC handshake with the platform's
        // classical one. Strip the marker before it leaves the device.
        return request.header(PQC_MODE_HEADER)?.equals("off", ignoreCase = true) != true
    }

    override fun onSkip(chain: Interceptor.Chain): Response {
        val cleaned = chain.request().newBuilder().removeHeader(PQC_MODE_HEADER).build()
        return chain.proceed(cleaned)
    }

    companion object {
        const val PQC_MODE_HEADER = "X-Pqc-Mode"
    }
}

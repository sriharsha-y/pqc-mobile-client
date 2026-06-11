import Foundation
import PqcCore

/// RN sample's `PqcURLProtocol` subclass. The base class (shipped in
/// `PqcCore`) handles the request/response plumbing; here we only
/// customise the `PqcConfig` (banking-style posture) and add the
/// `X-Pqc-Mode: off` toggle the sample UI uses to contrast PQC vs the
/// platform handshake.
@objc(RnSamplePqcURLProtocol)
public final class RnSamplePqcURLProtocol: PqcURLProtocol {

    /// Opt-in request header the sample sets to select the classical client.
    /// Stripped before the request leaves the device.
    static let pqcModeHeader = "X-Pqc-Mode"

    public override class func makeConfig() -> PqcConfig {
        // Banking-style overrides on top of URLSession defaults:
        //   - SameOriginOnly redirects (refuse cross-origin downgrades),
        //   - cache off (don't serve authenticated responses from cache),
        //   - 15 s total budget,
        //   - explicit UA for Akamai / bank WAFs.
        // Cookies are left ON (the platform default) so the Rust client's
        // jar tracks session cookies across requests through the protocol;
        // this matches Android and keeps fetch-based login flows working.
        // A real banking app MUST also populate pinnedDomains with per-host
        // base64(SHA-256(SPKI)) of the production leaf + next leaf.
        return .platformDefault(
            defaultTimeoutMs: 15_000,
            userAgent: "RnSample/0.3.1 (pqc-mobile-client)",
            redirectPolicy: .sameOriginOnly,
            enableCache: false
        )
    }

    public override class func shouldHandle(_ request: URLRequest) -> Bool {
        guard super.shouldHandle(request) else { return false }
        // X-Pqc-Mode: off bypasses PQC interception so the sample can
        // contrast the handshake with the iOS system stack.
        let header = request.value(forHTTPHeaderField: pqcModeHeader)
        return header?.caseInsensitiveCompare("off") != .orderedSame
    }
}

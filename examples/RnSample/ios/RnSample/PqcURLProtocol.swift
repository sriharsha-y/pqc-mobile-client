import Foundation
// PqcConfig / PqcHttpClient / HttpRequest / HttpResponse / HttpMethod are
// exported by the PqcCore Pod (which vendors the XCFramework and ships the
// UniFFI-generated pqc.swift).
import PqcCore

/// URLProtocol that intercepts NSURLSession requests and routes them
/// through the Rust `PqcHttpClient`. Register it via
/// `URLSessionConfiguration.protocolClasses` (see AppDelegate.mm).
///
/// In production, restrict interception to hosts that genuinely need PQC
/// TLS; intercepting every fetch() is overkill outside a sample.
@objc(PqcURLProtocol)
public final class PqcURLProtocol: URLProtocol {

    /// Sample intercepts every https URL; use a Set<String> of allowed
    /// hosts in a real app.
    private static let interceptAll = true

    // "off" makes the request skip interception and use the iOS system stack
    // (URLSession) — see canInit. Otherwise it's routed through the PQC client.
    static let pqcModeHeader = "X-Pqc-Mode"

    // The client always advertises the X25519MLKEM768 hybrid. The classical
    // path is the iOS system stack (see canInit), not a second client.
    //
    // NOTE: pinnedCertSha256 is [] here. A real banking app MUST populate it
    // with base64(SHA-256(SPKI)) for the production leaf (+ a pre-deployed
    // next leaf for rotation). See docs/ios.md §10.
    private static func makeClient() -> PqcHttpClient? {
        do {
            return try PqcHttpClient(
                config: PqcConfig(
                    pinnedCertSha256: [],
                    defaultTimeoutMs: 15_000,
                    // nil → 10s connect default. Set explicitly in production
                    // to survive a defaults change.
                    connectTimeoutMs: nil,
                    // Banking clients should not auto-attach cookies.
                    enableCookies: false,
                    // Identify to bank WAFs / Akamai Bot Manager.
                    userAgent: "RnSample/0.3.1 (pqc-mobile-client)",
                    // Refuse cross-origin redirects — they re-handshake to a
                    // host whose pin / PQ guarantees are independent.
                    redirectPolicy: .sameOriginOnly,
                    // Opt-in RFC 9111 response cache (off here). To enable, set
                    // enableCache: true and pass a Caches dir path; keep the
                    // URLProtocol's cacheStoragePolicy .notAllowed so the Rust
                    // cache is the single cache. See docs/ios.md.
                    enableCache: false,
                    cacheDir: nil,
                    maxCacheBytes: nil
                )
            )
        } catch {
            NSLog("PqcURLProtocol: PqcHttpClient init failed: \(error)")
            return nil
        }
    }

    // `pqc` prefix avoids shadowing URLProtocol's inherited `client`
    // property (the delegate we call back into via self.client?.urlProtocol).
    private static let pqcClient: PqcHttpClient? = makeClient()

    private var pqcTask: Task<Void, Never>?

    public override class func canInit(with request: URLRequest) -> Bool {
        guard request.url?.scheme == "https" else { return false }
        // Toggle "off": don't intercept, so the request goes through the iOS
        // system stack (URLSession) instead of this library — letting the
        // sample contrast the PQC handshake with the platform's classical one.
        if request.value(forHTTPHeaderField: pqcModeHeader)?
            .caseInsensitiveCompare("off") == .orderedSame {
            return false
        }
        return interceptAll
    }

    public override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        return request
    }

    public override func startLoading() {
        let req = self.request
        pqcTask = Task {
            do {
                guard let url = req.url else {
                    throw NSError(
                        domain: "PqcURLProtocol",
                        code: -1,
                        userInfo: [NSLocalizedDescriptionKey: "missing URL"]
                    )
                }
                // Requests carrying X-Pqc-Mode: off are not intercepted (see
                // canInit) — they use URLSession directly. Anything reaching
                // here therefore goes through the PQC client.
                let allHeaders = req.allHTTPHeaderFields ?? [:]
                guard let pqcClient = Self.pqcClient else {
                    throw NSError(
                        domain: "PqcURLProtocol",
                        code: -3,
                        userInfo: [
                            NSLocalizedDescriptionKey:
                                "PqcHttpClient unavailable — check init logs",
                        ]
                    )
                }
                // An unrecognized verb must FAIL loudly, not silently become
                // a GET — that would drop the body and turn a write into a
                // read with no error (the Android interceptor throws too).
                // nil httpMethod defaults to GET, matching URLSession.
                let method: HttpMethod
                if let raw = req.httpMethod {
                    guard let parsed = Self.parseMethod(raw) else {
                        throw NSError(
                            domain: "PqcURLProtocol",
                            code: -4,
                            userInfo: [
                                NSLocalizedDescriptionKey:
                                    "unsupported HTTP method: \(raw)",
                            ]
                        )
                    }
                    method = parsed
                } else {
                    method = .get
                }

                // Streamed / multipart / large uploads arrive via
                // httpBodyStream with httpBody nil; reading only httpBody
                // would send an empty payload, so drain the stream too.
                let body = req.httpBody ?? Self.drainBodyStream(req.httpBodyStream)

                // allHTTPHeaderFields is [String: String] (Apple already
                // comma-joins duplicates), so wrap each value in a 1-element
                // array for HttpRequest.headers' [String: [String]] shape.
                let forwardedHeaders = allHeaders
                    .filter { $0.key.caseInsensitiveCompare(Self.pqcModeHeader) != .orderedSame }
                    .mapValues { [$0] }
                let pqcReq = HttpRequest(
                    method: method,
                    url: url.absoluteString,
                    headers: forwardedHeaders,
                    body: body,
                    timeoutMs: nil
                )

                let pqcResp = try await pqcClient.request(req: pqcReq)

                // Authoritative URL for cookie scoping and response provenance:
                // the post-redirect URL the body actually came from. Falling
                // back to the request URL only if the Rust core's finalUrl is
                // unparseable (it never should be).
                let responseUrl = URL(string: pqcResp.finalUrl) ?? url

                // Handle Set-Cookie BEFORE building the response, and keep it
                // OUT of the response header dict. HTTPURLResponse is backed by
                // [String: String]; joining Set-Cookies with ", " corrupts them
                // (the `Expires` attribute itself contains a comma), so parse
                // each value in its OWN single-entry dict.
                //
                // SECURITY NOTE: this persists session cookies in
                // HTTPCookieStorage.shared (auto-attached to later requests),
                // mirroring normal iOS. For the Rust client's stricter
                // "no implicit cookie state" posture (enableCookies = false),
                // skip this and surface raw Set-Cookie to your app layer.
                let cookieStorage = HTTPCookieStorage.shared
                for (name, values) in pqcResp.headers where name.lowercased() == "set-cookie" {
                    for raw in values {
                        let parsed = HTTPCookie.cookies(
                            withResponseHeaderFields: ["Set-Cookie": raw],
                            for: responseUrl
                        )
                        for cookie in parsed { cookieStorage.setCookie(cookie) }
                    }
                }

                // Every header EXCEPT Set-Cookie (handled above). Comma-joining
                // the rest is RFC 9110 §5.3-legal for combinable fields.
                let headerFields = pqcResp.headers
                    .filter { $0.key.lowercased() != "set-cookie" }
                    .mapValues { values in values.joined(separator: ", ") }

                // Map the Rust core's `negotiated_protocol` (ALPN id) to a
                // value HTTPURLResponse accepts. Defaults to HTTP/1.1 on
                // unknown values rather than fabricating HTTP/2 — wrong
                // telemetry is worse than conservative.
                let httpVersion: String = {
                    switch pqcResp.negotiatedProtocol {
                    case "http/0.9", "http/1.0": return "HTTP/1.0"
                    case "http/1.1":             return "HTTP/1.1"
                    case "h2":                   return "HTTP/2.0"
                    case "h3":                   return "HTTP/3.0"
                    default:                     return "HTTP/1.1"
                    }
                }()
                guard let response = HTTPURLResponse(
                    url: responseUrl,
                    statusCode: Int(pqcResp.status),
                    httpVersion: httpVersion,
                    headerFields: headerFields
                ) else {
                    throw NSError(
                        domain: "PqcURLProtocol",
                        code: -2,
                        userInfo: [NSLocalizedDescriptionKey: "bad response construction"]
                    )
                }

                self.client?.urlProtocol(
                    self,
                    didReceive: response,
                    cacheStoragePolicy: .notAllowed
                )
                self.client?.urlProtocol(self, didLoad: Data(pqcResp.body))
                self.client?.urlProtocolDidFinishLoading(self)
            } catch {
                self.client?.urlProtocol(self, didFailWithError: error)
            }
        }
    }

    public override func stopLoading() {
        pqcTask?.cancel()
        pqcTask = nil
    }

    /// Read an InputStream-backed request body fully into memory.
    /// Returns nil when there is no stream. The sample materializes the
    /// whole body (the Rust core takes bytes, not a stream); a production
    /// integration with very large uploads should stream instead.
    private static func drainBodyStream(_ stream: InputStream?) -> Data? {
        guard let stream = stream else { return nil }
        stream.open()
        defer { stream.close() }
        var data = Data()
        let bufferSize = 64 * 1024
        var buffer = [UInt8](repeating: 0, count: bufferSize)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: bufferSize)
            if read < 0 { return nil } // stream error
            if read == 0 { break }     // EOF
            data.append(buffer, count: read)
        }
        return data
    }

    private static func parseMethod(_ s: String) -> HttpMethod? {
        switch s.uppercased() {
        case "GET":     return .get
        case "POST":    return .post
        case "PUT":     return .put
        case "DELETE":  return .delete
        case "PATCH":   return .patch
        case "HEAD":    return .head
        case "OPTIONS": return .options
        default:        return nil
        }
    }
}

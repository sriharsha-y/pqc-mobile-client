import Foundation
// PqcConfig / PqcHttpClient / HttpRequest / HttpResponse / HttpMethod
// are exported by the PqcCore Pod (which vendors PqcCore.xcframework
// and ships UniFFI-generated pqc.swift inside its module). pqc.swift
// internally imports the `pqcFFI` C module declared by the
// XCFramework's module.modulemap.
import PqcCore

/// URLProtocol that intercepts NSURLSession requests and routes them
/// through the Rust `PqcHttpClient`. Register it via
/// `URLSessionConfiguration.protocolClasses` on RN's session config
/// (see AppDelegate.mm).
///
/// In production, restrict `pqcHosts` to the hostnames that genuinely need
/// PQC TLS (banking APIs); leaving "*" wildcards means every fetch() in
/// the app goes through the Rust core, which is overkill outside a sample.
@objc(PqcURLProtocol)
public final class PqcURLProtocol: URLProtocol {

    /// For the sample we intercept every https URL. Replace with a Set<String>
    /// of allowed hosts in a real app.
    private static let interceptAll = true

    // Opt-in request header the RN sample sets to select the
    // classical-only client ("off"). Stripped before the request leaves
    // the device.
    static let pqcModeHeader = "X-Pqc-Mode"

    // Shared config differing only in enablePostQuantum. A production app
    // needs only the PQC client; the sample keeps both so the UI can
    // toggle PQC on/off (the flag is fixed at client construction).
    //
    // NOTE: pinnedCertSha256 is [] in the sample. A real banking app
    // MUST populate this with base64(SHA-256(SPKI)) for the production
    // leaf cert (+ a pre-deployed next leaf for rotation). See
    // pqc-mobile-client/docs/ios.md §10.
    private static func makeClient(enablePqc: Bool) -> PqcHttpClient? {
        do {
            return try PqcHttpClient(
                config: PqcConfig(
                    pinnedCertSha256: [],
                    enablePostQuantum: enablePqc,
                    defaultTimeoutMs: 15_000,
                    // nil → built-in defaults (10s connect, 16 MiB body
                    // cap). Set explicitly in production to survive a
                    // defaults change.
                    connectTimeoutMs: nil,
                    maxBodyBytes: nil,
                    // Banking clients should not auto-attach cookies.
                    enableCookies: false,
                    // Identify to bank WAFs / Akamai Bot Manager.
                    userAgent: "RnSample/0.3.1 (pqc-mobile-client)",
                    // Cross-origin redirects would re-handshake to a
                    // different host whose pin / PQ guarantees are
                    // independent — refuse them.
                    redirectPolicy: .sameOriginOnly
                )
            )
        } catch {
            NSLog("PqcURLProtocol: PqcHttpClient init failed: \(error)")
            return nil
        }
    }

    // Named with a `pqc` prefix rather than `client` to avoid shadowing
    // URLProtocol's inherited instance property `client: URLProtocolClient?`
    // (the loading-system delegate we call back into via
    // self.client?.urlProtocol(...) below).
    private static let pqcClient: PqcHttpClient? = makeClient(enablePqc: true)
    private static let classicalClient: PqcHttpClient? = makeClient(enablePqc: false)

    private var pqcTask: Task<Void, Never>?

    public override class func canInit(with request: URLRequest) -> Bool {
        guard request.url?.scheme == "https" else { return false }
        if URLProtocol.property(forKey: "PqcHandled", in: request) as? Bool == true {
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
                // Route on the opt-in mode header: "off" selects the
                // classical-only client. Strip the header below so it
                // never leaves the device.
                let allHeaders = req.allHTTPHeaderFields ?? [:]
                let modeValue = allHeaders.first {
                    $0.key.caseInsensitiveCompare(Self.pqcModeHeader) == .orderedSame
                }?.value
                let classicalOnly = modeValue?.caseInsensitiveCompare("off") == .orderedSame
                guard let pqcClient = classicalOnly ? Self.classicalClient : Self.pqcClient else {
                    throw NSError(
                        domain: "PqcURLProtocol",
                        code: -3,
                        userInfo: [
                            NSLocalizedDescriptionKey:
                                "PqcHttpClient unavailable — check init logs",
                        ]
                    )
                }
                // Resolve the HTTP method. An unrecognized verb must FAIL
                // loudly, not silently become a GET — defaulting to GET
                // would drop the body and turn an intended write into a
                // read with no error surfaced (the Android interceptor
                // throws on the same input). nil httpMethod defaults to
                // GET, matching URLSession's own default.
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

                // URLSession delivers streamed / multipart / large upload
                // bodies via httpBodyStream, leaving httpBody nil. Reading
                // only httpBody would silently send an empty payload for
                // those uploads. Drain the stream when present.
                let body = req.httpBody ?? Self.drainBodyStream(req.httpBodyStream)

                // URLRequest.allHTTPHeaderFields is [String: String] — Apple
                // already comma-joins duplicate headers upstream, so we wrap
                // each value in a single-element array to satisfy the
                // [String: [String]] shape on HttpRequest.headers.
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

                // --- Cookies: handle Set-Cookie BEFORE building the
                // response, and keep them OUT of the response header dict.
                //
                // iOS HTTPURLResponse is backed by a [String: String]
                // dictionary, which physically cannot hold more than one
                // value per header name. The usual workaround — joining
                // multiple Set-Cookie values with ", " — corrupts them,
                // because a cookie's `Expires` attribute itself contains a
                // comma (e.g. "Expires=Wed, 21 Oct 2026 ..."), so anything
                // re-splitting on commas mis-parses the boundary.
                //
                // The Rust core preserves each Set-Cookie value as its own
                // entry, so handle them explicitly: parse each value on its
                // OWN (a single-entry dict per cookie sidesteps the comma
                // ambiguity entirely) and hand the parsed cookies to the
                // store the URL Loading System / RN networking reads from.
                // This is the correct iOS pattern for a synthesizing
                // URLProtocol; the response header dict then carries
                // everything EXCEPT Set-Cookie.
                //
                // SECURITY NOTE for banking integrators: this persists
                // session cookies in HTTPCookieStorage.shared, i.e. they
                // auto-attach to later requests. That mirrors normal iOS
                // behavior, but if you want the Rust client's stricter
                // "no implicit cookie state" posture (PqcConfig.enableCookies
                // = false), skip this block and instead surface the raw
                // Set-Cookie values to your app layer to decide per request.
                let cookieStorage = HTTPCookieStorage.shared
                for (name, values) in pqcResp.headers where name.lowercased() == "set-cookie" {
                    for raw in values {
                        let parsed = HTTPCookie.cookies(
                            withResponseHeaderFields: ["Set-Cookie": raw],
                            for: url
                        )
                        for cookie in parsed { cookieStorage.setCookie(cookie) }
                    }
                }

                // Build the response header dict from every header EXCEPT
                // Set-Cookie (handled above). Comma-joining the remaining
                // headers is RFC 9110 §5.3-legal for combinable fields.
                let headerFields = pqcResp.headers
                    .filter { $0.key.lowercased() != "set-cookie" }
                    .mapValues { values in values.joined(separator: ", ") }

                // Map the Rust core's `negotiated_protocol` string (the
                // ALPN protocol id — "h2", "http/1.1", etc.) into a value
                // HTTPURLResponse will accept. Defaults to HTTP/1.1 on
                // unknown values rather than fabricating HTTP/2 — wrong
                // telemetry is worse than conservative telemetry.
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
                    url: url,
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

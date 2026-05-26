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

    // Optional because PqcHttpClient init now throws on bad config
    // (e.g. malformed base64 pin entries). If construction fails we
    // log and leave the client nil — startLoading then synthesizes a
    // network-failure error rather than crashing the app.
    //
    // NOTE: pinnedCertSha256 is [] in the sample. A real banking app
    // MUST populate this with base64(SHA-256(SPKI)) for the production
    // leaf cert (+ a pre-deployed next leaf for rotation). See
    // pqc-mobile-client/ios/README.md §10.
    // Named `httpClient` rather than `client` to avoid shadowing
    // URLProtocol's inherited instance property `client: URLProtocolClient?`
    // (the loading-system delegate we call back into via
    // self.client?.urlProtocol(...) below).
    private static let httpClient: PqcHttpClient? = {
        do {
            return try PqcHttpClient(
                config: PqcConfig(
                    pinnedCertSha256: [],
                    enablePostQuantum: true,
                    enableHttp3: false,
                    defaultTimeoutMs: 15_000
                )
            )
        } catch {
            NSLog("PqcURLProtocol: PqcHttpClient init failed: \(error)")
            return nil
        }
    }()

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
                guard let pqcClient = Self.httpClient else {
                    throw NSError(
                        domain: "PqcURLProtocol",
                        code: -3,
                        userInfo: [
                            NSLocalizedDescriptionKey:
                                "PqcHttpClient unavailable — check init logs",
                        ]
                    )
                }
                // URLRequest.allHTTPHeaderFields is [String: String] — Apple
                // already comma-joins duplicate headers upstream, so we wrap
                // each value in a single-element array to satisfy the
                // [String: [String]] shape on HttpRequest.headers.
                let pqcReq = HttpRequest(
                    method: req.httpMethod.flatMap(Self.parseMethod) ?? .get,
                    url: url.absoluteString,
                    headers: (req.allHTTPHeaderFields ?? [:]).mapValues { [$0] },
                    body: req.httpBody,
                    timeoutMs: nil
                )

                let pqcResp = try await pqcClient.request(req: pqcReq)

                // Build a header dict that includes the negotiated KEX so
                // JS can verify the handshake via response.headers.get(...).
                var headerFields = pqcResp.headers.mapValues { values in
                    values.joined(separator: ", ")
                }
                headerFields["X-Pqc-Negotiated-Group"] = pqcResp.negotiatedNamedGroup

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

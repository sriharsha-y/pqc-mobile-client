import Foundation
// PqcConfig / PqcHttpClient / HttpRequest / HttpResponse / HttpMethod
// come from pqc.swift (UniFFI-generated), which is added to the same
// app target. pqc.swift itself imports the `pqcFFI` C module declared
// by the bundled PqcClient.xcframework's module.modulemap.

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

    private static let client: PqcHttpClient = {
        PqcHttpClient(
            config: PqcConfig(
                pinnedCertSha256: [],
                enablePostQuantum: true,
                enableHttp3: false,
                defaultTimeoutMs: 15_000
            )
        )
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
                let pqcReq = HttpRequest(
                    method: req.httpMethod.flatMap(Self.parseMethod) ?? .get,
                    url: url.absoluteString,
                    headers: req.allHTTPHeaderFields ?? [:],
                    body: req.httpBody,
                    timeoutMs: nil
                )

                let pqcResp = try await Self.client.request(req: pqcReq)

                // Build a header dict that includes the negotiated KEX so
                // JS can verify the handshake via response.headers.get(...).
                var headerFields = pqcResp.headers.mapValues { values in
                    values.joined(separator: ", ")
                }
                headerFields["X-Pqc-Negotiated-Group"] = pqcResp.negotiatedNamedGroup

                guard let response = HTTPURLResponse(
                    url: url,
                    statusCode: Int(pqcResp.status),
                    httpVersion: "HTTP/1.1",
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

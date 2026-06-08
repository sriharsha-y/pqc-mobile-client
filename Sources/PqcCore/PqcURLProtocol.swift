import Foundation

/// `URLProtocol` subclass routing HTTPS through the Rust `PqcHttpClient`
/// for an X25519MLKEM768 hybrid handshake. Subclass and override
/// ``makeConfig()``, ``shouldHandle(_:)``, or ``mapError(_:)``.
///
/// **Cookies and response cache stay in the Rust client.** `Cookie:` headers
/// on the inbound `URLRequest` are stripped (URLSession may or may not
/// pre-attach for custom URLProtocols — undocumented), `Set-Cookie:` is
/// dropped from the synthesised `HTTPURLResponse` (the `[String: String]`
/// backing corrupts multi-value Set-Cookies anyway), and `cacheStoragePolicy`
/// is hard-coded to `.notAllowed`. The Rust client's own jar / RFC 9111
/// cache is the single source of truth.
// `Task { ... }` / `async` require iOS 13 / macOS 10.15. iOS 13 floor comes
// from the package; the macOS bound is needed because SPM defaults macOS
// to an older floor when the package doesn't list macOS explicitly.
@available(iOS 13.0, macOS 10.15, *)
@objc(PqcURLProtocol)
open class PqcURLProtocol: URLProtocol {

    // MARK: - Override points

    /// `PqcConfig` for this subclass's shared client. Default: ``PqcConfig/platformDefault()``.
    open class func makeConfig() -> PqcConfig { .platformDefault() }

    /// Whether the protocol claims a given request. Default: HTTPS only.
    open class func shouldHandle(_ request: URLRequest) -> Bool {
        request.url?.scheme?.lowercased() == "https"
    }

    /// Map errors before they reach the `URLSession` consumer. Default
    /// translates `PqcError` variants to the closest `URLError` codes
    /// (e.g. `.PinningFailure` → `.serverCertificateUntrusted`).
    open class func mapError(_ error: Error) -> Error {
        if let pqc = error as? PqcError {
            switch pqc {
            case .Timeout:           return URLError(.timedOut)
            case .PinningFailure:    return URLError(.serverCertificateUntrusted)
            case .TrustVerification: return URLError(.serverCertificateUntrusted)
            case .Network:           return URLError(.networkConnectionLost)
            case .Tls:               return URLError(.secureConnectionFailed)
            case .InvalidRequest:    return URLError(.badURL)
            }
        }
        return error
    }

    // MARK: - Convenience

    /// Insert at index 0 of `configuration.protocolClasses`. Inserts
    /// unconditionally — see ``registerIfNeeded(into:)`` for the gated form.
    ///
    /// `@objc(...)` so ObjC++ consumers (RN `AppDelegate.mm`) can call it
    /// as `[MySubclass registerInto:cfg]`; class-level `@objc` doesn't
    /// propagate to static funcs in the generated `-Swift.h`.
    @objc(registerInto:)
    public static func register(into configuration: URLSessionConfiguration) {
        var classes = configuration.protocolClasses ?? []
        classes.insert(self as AnyClass, at: 0)
        configuration.protocolClasses = classes
    }

    /// Like ``register(into:)`` but no-ops on iOS 26+ / macOS 15+ where
    /// URLSession already negotiates X25519MLKEM768 natively
    /// (Apple [HT122756](https://support.apple.com/en-us/122756)).
    /// Call ``register(into:)`` directly if you need SPKI pinning on those
    /// versions too.
    @objc(registerIfNeededInto:)
    public static func registerIfNeeded(into configuration: URLSessionConfiguration) {
        if #available(iOS 26.0, macOS 15.0, *) { return }
        register(into: configuration)
    }

    // MARK: - Per-subclass shared `PqcHttpClient`

    /// Shared `PqcHttpClient` for the given concrete subclass; built once
    /// via `type.makeConfig()` and cached for the process lifetime.
    public static func clientFor(_ type: PqcURLProtocol.Type) throws -> PqcHttpClient {
        let key = ObjectIdentifier(type)
        clientCacheLock.lock()
        defer { clientCacheLock.unlock() }
        if let existing = clientCache[key] { return existing }
        let client = try PqcHttpClient(config: type.makeConfig())
        clientCache[key] = client
        return client
    }

    private static let clientCacheLock = NSLock()
    private static var clientCache: [ObjectIdentifier: PqcHttpClient] = [:]

    // MARK: - `URLProtocol` overrides (final implementations)

    public override class func canInit(with request: URLRequest) -> Bool {
        shouldHandle(request)
    }

    public override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        request
    }

    private var pqcTask: Task<Void, Never>?
    /// Captured once `client.request(...)` returns so `stopLoading()` can
    /// invoke `cancel()` on it. UniFFI 0.29 doesn't propagate Swift
    /// `Task.cancel()` into Rust (mozilla/uniffi-rs#2771), so without this
    /// the underlying request keeps streaming bytes and holding the global +
    /// per-host semaphore permits long after URLSession told us to stop.
    private var pqcResp: PqcResponse?

    public override func startLoading() {
        let subclass = type(of: self)
        pqcTask = Task {
            do {
                guard let url = self.request.url else {
                    throw PqcError.InvalidRequest(message: "missing URL")
                }
                let client = try subclass.clientFor(subclass)
                let pqcReq = try Self.buildRequest(from: self.request, url: url)
                let resp = try await client.request(req: pqcReq)
                self.pqcResp = resp
                try await self.emit(resp, originalURL: url)
            } catch is CancellationError {
                // stopLoading() cancelled us; URLProtocol contract is to
                // stay silent — URLSession owns the cancel notification.
            } catch {
                self.client?.urlProtocol(self, didFailWithError: subclass.mapError(error))
            }
        }
    }

    public override func stopLoading() {
        let resp = self.pqcResp
        self.pqcResp = nil
        pqcTask?.cancel()
        pqcTask = nil
        // Sync FFI call — releases the connection + permits NOW. No
        // detached Task, no async dance. `cancel()` is idempotent and
        // races safely with an `emit()` already in flight (the read
        // loop observes `cancelled` at its next chunk boundary).
        resp?.cancel()
    }

    // MARK: - Request / response plumbing

    private static func buildRequest(from req: URLRequest, url: URL) throws -> HttpRequest {
        let rawMethod = req.httpMethod ?? "GET"
        guard let method = parseMethod(rawMethod) else {
            throw PqcError.InvalidRequest(message: "unsupported HTTP method: \(rawMethod)")
        }

        // Body routing matches Android: small inline `httpBody` is buffered
        // once into Data; `httpBodyStream` (the URLSession streaming
        // upload primitive) is wrapped as a BodyProvider so the Rust core
        // pulls chunks via FFI without ever materializing the full
        // upload — matches URLSession's own streamed-request behavior.
        let inlineBody: Data?
        let streamProvider: BodyProvider?
        let streamLength: UInt64?
        if let body = req.httpBody {
            inlineBody = body
            streamProvider = nil
            streamLength = nil
        } else if let stream = req.httpBodyStream {
            inlineBody = nil
            streamProvider = InputStreamBodyProvider(stream: stream)
            // URLRequest puts a known length in Content-Length when the
            // caller set one; otherwise reqwest will use chunked
            // transfer-encoding. URLSession honors this same contract
            // (see Apple HT122756 / URLSession needNewBodyStream docs).
            streamLength = (req.value(forHTTPHeaderField: "Content-Length"))
                .flatMap(UInt64.init)
        } else {
            inlineBody = nil
            streamProvider = nil
            streamLength = nil
        }

        // allHTTPHeaderFields is [String: String]; wrap each value in a
        // 1-element array. Strip Cookie: (Rust jar is authoritative).
        // Strip Content-Length when we set it via bodyStreamLength —
        // the Rust client re-emits it and forwarding both would
        // duplicate the header.
        var headers: [String: [String]] = [:]
        for (key, value) in req.allHTTPHeaderFields ?? [:] {
            if key.caseInsensitiveCompare("Cookie") == .orderedSame { continue }
            if streamLength != nil && key.caseInsensitiveCompare("Content-Length") == .orderedSame {
                continue
            }
            headers[key] = [value]
        }

        return HttpRequest(
            method: method,
            url: url.absoluteString,
            headers: headers,
            body: inlineBody,
            bodyStream: streamProvider,
            bodyStreamLength: streamLength,
            timeoutMs: nil
        )
    }

    /// `Set-Cookie` is dropped because `HTTPURLResponse`'s `[String: String]`
    /// header backing corrupts comma-bearing Expires dates — the Rust jar
    /// owns cookies anyway. Body is streamed chunk-by-chunk so large
    /// responses don't materialise in memory.
    private func emit(_ pqcResp: PqcResponse, originalURL: URL) async throws {
        let responseURL = URL(string: pqcResp.finalUrl()) ?? originalURL

        var headerFields: [String: String] = [:]
        for (key, values) in pqcResp.headers() {
            if key.lowercased() == "set-cookie" { continue }
            headerFields[key] = values.joined(separator: ", ")
        }

        let httpVersion = Self.httpVersionString(forAlpn: pqcResp.negotiatedProtocol())

        guard let response = HTTPURLResponse(
            url: responseURL,
            statusCode: Int(pqcResp.status()),
            httpVersion: httpVersion,
            headerFields: headerFields
        ) else {
            throw PqcError.InvalidRequest(message: "failed to construct HTTPURLResponse")
        }

        self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        while let chunk = try await pqcResp.readChunk() {
            self.client?.urlProtocol(self, didLoad: Data(chunk))
        }
        self.client?.urlProtocolDidFinishLoading(self)
    }

    // MARK: - Small helpers (file-private)

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

    /// Maps ALPN id → `HTTPURLResponse.httpVersion`. Unknown ALPN falls
    /// back to HTTP/1.1 (conservative is better than wrong telemetry).
    private static func httpVersionString(forAlpn alpn: String) -> String {
        switch alpn {
        case "http/0.9", "http/1.0": return "HTTP/1.0"
        case "http/1.1":             return "HTTP/1.1"
        case "h2":                   return "HTTP/2.0"
        case "h3":                   return "HTTP/3.0"
        default:                     return "HTTP/1.1"
        }
    }
}

/// Adapts a Foundation `InputStream` (as exposed by
/// `URLRequest.httpBodyStream`) to the Rust `BodyProvider` PULL
/// contract. Each `nextChunk` call reads up to 64 KiB synchronously
/// — the Rust client invokes this on a tokio `spawn_blocking` worker
/// so blocking stream reads are safe.
///
/// Lifecycle: opens the stream on first read, closes on EOF or
/// deallocation. Reading from a closed stream returns `nil` (EOF) so
/// the Rust side terminates cleanly.
///
/// Threading: `nextChunk` is sequential from one Rust worker; `cancel()`
/// may race it from another. The NSLock serializes the two. We don't
/// close out from under a concurrent read — Foundation `InputStream`
/// is not thread-safe. `@unchecked Sendable` because UniFFI's
/// `BodyProvider` is Sendable but `InputStream` isn't.
final class InputStreamBodyProvider: BodyProvider, @unchecked Sendable {
    private let stream: InputStream
    private var opened = false
    private var closed = false
    private let lock = NSLock()

    init(stream: InputStream) {
        self.stream = stream
    }

    deinit {
        // Safety net in case cancel() isn't called (e.g. an old build
        // of the Rust client). Holds the lock to make sure no
        // concurrent nextChunk is mid-read on the InputStream.
        lock.lock()
        defer { lock.unlock() }
        closeIfOpenLocked()
    }

    func cancel() {
        // Idempotent. Waits for any in-flight nextChunk to return before
        // closing (see type doc for why we don't interrupt a read).
        lock.lock()
        defer { lock.unlock() }
        closeIfOpenLocked()
    }

    /// Caller must hold `lock`. Idempotent: marks `closed` even if the
    /// stream was never opened, so subsequent `nextChunk` short-circuits.
    private func closeIfOpenLocked() {
        if opened && !closed {
            stream.close()
        }
        closed = true
    }

    func nextChunk() throws -> Data? {
        lock.lock()
        defer { lock.unlock() }
        if closed { return nil }
        if !opened {
            stream.open()
            opened = true
        }
        // 64 KiB matches Rust-side STREAM_CHUNK_SIZE and Android's
        // OkHttpBodyProviderAdapter chunk size — keeps memory bounded
        // and amortizes syscall cost.
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        let n = stream.read(&buf, maxLength: buf.count)
        if n < 0 {
            let underlying = stream.streamError?.localizedDescription
                ?? "stream read returned an error"
            closeIfOpenLocked()
            throw PqcError.InvalidRequest(
                message: "request body stream read failed: \(underlying)"
            )
        }
        if n == 0 {
            closeIfOpenLocked()
            return nil  // EOF
        }
        return Data(bytes: buf, count: n)
    }
}

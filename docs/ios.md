# iOS consumption guide

`pqc_client` on iOS, consumed from:

- **A native iOS app** using `URLSession` (Sections 3 and 4)
- **A native iOS app** using a custom HTTP client or no HTTP framework (Section 5)
- **A React Native iOS app** (Section 6)

The Rust core, the XCFramework, and the generated Swift bindings are the same regardless of consumer.

## 1. Build outputs

> **Note on regenerating bindings manually.** The build script invokes
> `cargo run --release --features cli --bin uniffi-bindgen -- generate ...`.
> The `--features cli` flag is mandatory — the uniffi-bindgen binary is
> gated behind a `cli` cargo feature so its dep tree (clap, goblin,
> uniffi_bindgen itself) never gets linked into the mobile cross-compiled
> archive. Running `cargo run --bin uniffi-bindgen ...` without the flag
> errors with `target uniffi-bindgen requires the features: cli`.

After `make ios` at the repo root:

```
generated/
├── PqcCore.xcframework         (arm64 device + arm64/x86_64 simulator)
└── swift/
    ├── pqc.swift               (UniFFI-generated Swift bindings)
    ├── pqcFFI.h
    └── module.modulemap
```

Binary footprint per arch: ~5–8 MB in the device IPA after App Store thinning.

## 2. Packaging

### CocoaPods (recommended for RN apps; works for native)

The pod is published to the CocoaPods Trunk registry on every release. In the consumer's `Podfile`:

```ruby
pod 'PqcCore', '~> 0.5.2' # x-release-please-version
```

`pod install` resolves through Trunk, downloads `PqcCore-X.Y.Z.zip` (XCFramework + Swift bindings) from the matching GitHub Release, and wires it in. No local build of this repo required.

Alternative (no Trunk dependency) — pin directly to the raw podspec URL at a release tag:

```ruby
pod 'PqcCore', :podspec => 'https://raw.githubusercontent.com/sriharsha-y/pqc-mobile-client/v0.5.2/PqcCore.podspec' # x-release-please-version
```

Useful when the consumer's CocoaPods setup can't reach Trunk (corporate firewalls, custom mirrors), or to pin to a specific tag that hasn't been Trunk-pushed yet.

### Swift Package Manager (recommended for native iOS apps)

In your app's `Package.swift`:

```swift
dependencies: [
    .package(url: "https://github.com/sriharsha-y/pqc-mobile-client.git", from: "0.5.2") // x-release-please-version
],
targets: [
    .target(
        name: "MyApp",
        dependencies: [
            .product(name: "PqcCore", package: "pqc-mobile-client"),
        ]
    )
]
```

Or in Xcode: **File → Add Package Dependencies…** → paste the repo URL → pick "Up to Next Minor".

Behind the scenes: SPM resolves the version you pin to the matching `vX.Y.Z` git tag, which points at a commit on `main` where `Package.swift` lives at the repo root. That manifest declares `PqcCore.xcframework` as a `binaryTarget` whose URL fetches the release asset (`PqcCore-X.Y.Z.zip`) and SPM verifies its SHA256 checksum at download time — SPM finds the `.xcframework` at the zip root and ignores the bundled `pqc.swift`/LICENSE. CocoaPods consumes the same zip over the same HTTPS release endpoint but does **not** verify a per-pod-spec SHA256 — integrity in the CocoaPods path relies on HTTPS transport security and GitHub's write controls on the release asset. If you need byte-level integrity on the CocoaPods side too, prefer the SPM path or vendor the XCFramework manually.

`Package.swift` at the repo root is auto-maintained by the release workflow's `publish-swiftpm` job, which rewrites it with the latest version + URL + checksum on every release and re-points the release tag to the resulting commit.

## 3. Native iOS — `URLSession` via `URLProtocol` (drop-in)

`URLProtocol` is the iOS hook. A subclass intercepts requests for chosen hosts; the rest of the app keeps using `URLSession` unchanged.

```swift
import Foundation
import PqcCore        // UniFFI module

final class PqcURLProtocol: URLProtocol {
    static let pqcHosts: Set<String> = [
        "api.example.com",
        "auth.example.com",
        // ... full hostname list to route through PQC
    ]

    static let client: PqcHttpClient? = {
        // The PqcHttpClient constructor throws on malformed config
        // (e.g. bad base64 in pinnedCertSha256). Wrap in try? and
        // gracefully degrade rather than crashing the app.
        try? PqcHttpClient(
            config: PqcConfig(
                pinnedCertSha256: CertPins.spkiSha256,
                enablePostQuantum: true,
                defaultTimeoutMs: 15_000,
                connectTimeoutMs: nil,           // 10s default
                maxBodyBytes: nil,               // 16 MiB default
                enableCookies: false,            // banking: no auto cookie jar
                userAgent: "MyApp/1.0",          // identify to bank WAF / Akamai
                redirectPolicy: .sameOriginOnly  // refuse cross-origin 3xx
            )
        )
    }()

    private var task: Task<Void, Never>?

    override class func canInit(with request: URLRequest) -> Bool {
        guard request.url?.scheme == "https",
              let host = request.url?.host,
              Self.pqcHosts.contains(host) else { return false }
        if URLProtocol.property(forKey: "PqcHandled", in: request) as? Bool == true { return false }
        return true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let req = self.request
        task = Task {
            do {
                let pqcReq = HttpRequest(
                    method: req.httpMethod.flatMap(HttpMethod.from) ?? .get,
                    url: req.url!.absoluteString,
                    headers: (req.allHTTPHeaderFields ?? [:]).mapValues { [$0] },
                    body: req.httpBody,
                    timeoutMs: nil
                )
                guard let pqcClient = Self.client else {
                    throw NSError(domain: "PqcURLProtocol", code: -3,
                                  userInfo: [NSLocalizedDescriptionKey: "PqcHttpClient unavailable"])
                }
                let pqcResp = try await pqcClient.request(req: pqcReq)
                let nsResp = HTTPURLResponse(
                    url: req.url!,
                    statusCode: Int(pqcResp.status),
                    httpVersion: "HTTP/1.1",
                    headerFields: pqcResp.headers.mapValues { $0.joined(separator: ", ") }
                )!
                self.client?.urlProtocol(self, didReceive: nsResp, cacheStoragePolicy: .notAllowed)
                self.client?.urlProtocol(self, didLoad: Data(pqcResp.body))
                self.client?.urlProtocolDidFinishLoading(self)
            } catch {
                self.client?.urlProtocol(self, didFailWithError: error)
            }
        }
    }

    override func stopLoading() { task?.cancel(); task = nil }
}

private extension HttpMethod {
    static func from(_ s: String) -> HttpMethod? {
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
```

### Registration — native app

Two paths depending on which session the app uses.

**Pattern A — `URLSession.shared`:** `URLProtocol.registerClass(...)` works directly.

```swift
if #available(iOS 26, *) {
    // Native URLSession already negotiates X25519MLKEM768. No-op.
} else {
    URLProtocol.registerClass(PqcURLProtocol.self)
}
```

**Pattern B — a custom `URLSession`:** add the class to the session's `configuration.protocolClasses` before constructing the session.

```swift
let cfg = URLSessionConfiguration.default
if #unavailable(iOS 26, *) {
    cfg.protocolClasses = [PqcURLProtocol.self] + (cfg.protocolClasses ?? [])
}
let session = URLSession(configuration: cfg)
```

Existing API code (Alamofire, Moya, raw `URLSession.dataTask`) using this session continues to work unchanged.

### Cookies & multi-value response headers

`HTTPURLResponse` is backed by a `[String: String]` dictionary, so it **cannot carry more than one value for a header name**. The common shortcut of joining multiple `Set-Cookie` headers with `", "` corrupts them, because a cookie's `Expires` attribute itself contains a comma (`Expires=Wed, 21 Oct 2026 ...`) — any later comma-split mis-parses the boundary and drops or mangles cookies.

The Rust core preserves each `Set-Cookie` as its own value, so a synthesizing `URLProtocol` must handle cookies explicitly rather than fold them into the response dict. Parse each value **on its own** (a single-entry dict per cookie avoids the comma ambiguity) and hand it to the cookie store the URL Loading System / RN networking reads from:

```swift
let cookieStorage = HTTPCookieStorage.shared
for (name, values) in pqcResp.headers where name.lowercased() == "set-cookie" {
    for raw in values {
        let parsed = HTTPCookie.cookies(withResponseHeaderFields: ["Set-Cookie": raw], for: url)
        for cookie in parsed { cookieStorage.setCookie(cookie) }
    }
}

// Build the response header dict from everything EXCEPT Set-Cookie:
var headerFields = pqcResp.headers
    .filter { $0.key.lowercased() != "set-cookie" }
    .mapValues { $0.joined(separator: ", ") }
```

Comma-joining the **remaining** headers is fine — RFC 9110 §5.3 permits combining most field values with commas; `Set-Cookie` is the notable exception.

**Banking posture:** the snippet above persists session cookies in `HTTPCookieStorage.shared`, so they auto-attach to later requests (normal iOS behavior). If you want the Rust client's stricter "no implicit cookie state" stance (`PqcConfig.enableCookies = false`), **skip the storage step** and instead surface the raw `Set-Cookie` values to your app layer to decide per request. Either way, never comma-join `Set-Cookie`.

See `examples/RnSample/ios/RnSample/PqcURLProtocol.swift` for the full working implementation.

## 4. Native iOS — Alamofire / Moya / async-http-client

Alamofire and Moya wrap `URLSession`, so they inherit the URLProtocol hook from Section 3 if the underlying session is the one with `PqcURLProtocol` registered. Construct Alamofire's `Session` with a `URLSessionConfiguration` that includes the protocol class:

```swift
let cfg = URLSessionConfiguration.default
if #unavailable(iOS 26, *) {
    cfg.protocolClasses = [PqcURLProtocol.self] + (cfg.protocolClasses ?? [])
}
let af = Session(configuration: cfg)
```

`swift-nio`-based clients (AsyncHTTPClient) do not use `URLSession`; for those, call `PqcHttpClient` directly (Section 5).

## 5. Native iOS — direct use, no URLSession

For new code paths or non-URLSession-based clients, call `PqcHttpClient` directly. The UniFFI-generated Swift class has Swift-native `async`/`throws`.

```swift
let pqc = try PqcHttpClient(
    config: PqcConfig(
        pinnedCertSha256: [],
        enablePostQuantum: true,
        defaultTimeoutMs: 10_000,
        connectTimeoutMs: nil,           // 10s default
        maxBodyBytes: nil,               // 16 MiB default
        enableCookies: false,
        userAgent: "MyApp/1.0",
        redirectPolicy: .sameOriginOnly
    )
)

func fetchBalance() async throws -> Data {
    let resp = try await pqc.request(req: HttpRequest(
        method: .get,
        url: "https://api.bank.example/accounts/123/balance",
        headers: ["Authorization": ["Bearer \(token)"]],
        body: nil,
        timeoutMs: nil
    ))
    return Data(resp.body)
}
```

## 6. React Native iOS

`URLProtocol.registerClass(...)` does **NOT** work for React Native — `RCTHTTPRequestHandler` constructs its own `NSURLSession`, not `URLSession.shared`. The supported hook is `RCTSetCustomNSURLSessionConfigurationProvider`, called once during app launch.

```swift
// ios/AppDelegate.swift
import React
import PqcCore

@main
final class AppDelegate: RCTAppDelegate {
    override func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        RCTSetCustomNSURLSessionConfigurationProvider {
            let cfg = URLSessionConfiguration.default
            if #available(iOS 26, *) {
                // native PQC
            } else {
                cfg.protocolClasses = [PqcURLProtocol.self] + (cfg.protocolClasses ?? [])
            }
            return cfg
        }
        return super.application(application, didFinishLaunchingWithOptions: launchOptions)
    }
}
```

The `PqcURLProtocol` class is identical to the native case (Section 3).

## 7. iOS 26 gate

The `if #available(iOS 26, *)` check is the only runtime switch. On iOS 26+, the native `URLSession` already advertises `X25519MLKEM768` in every ClientHello (Apple [HT122756](https://support.apple.com/en-us/122756)), so the custom path is unnecessary and slightly slower. Skip registration on iOS 26+.

## 8. Export compliance

Bundling Rust crypto promotes the app from "uses-OS-encryption-only" (exempt) to "uses non-exempt encryption":

- `Info.plist`: `ITSAppUsesNonExemptEncryption = YES`
- File the annual self-classification report (ERN) with U.S. BIS — see [Apple's guide](https://developer.apple.com/documentation/security/complying-with-encryption-export-regulations).
- ML-KEM is FIPS 203 and qualifies for the standard TLS export exemption (no CCATS needed).
- Apache-2.0 attribution for `aws-lc-rs`, `rustls`, `reqwest` in the app's acknowledgements.

## 9. Verification

Debug-build sanity check. `HttpResponse` deliberately does not expose the negotiated key-exchange group (it is a per-connection property the client can only observe via a racy process-global — see the `HttpResponse` doc in `src/types.rs`). Confirm it from the **server's** report instead: Cloudflare's `/cdn-cgi/trace` returns a `kex=` line.

```swift
Task {
    let client = try PqcHttpClient(config: PqcConfig(/* … */))
    let resp = try await client.request(req: HttpRequest(
        method: .get,
        url: "https://pq.cloudflareresearch.com/cdn-cgi/trace",
        headers: [:] as [String: [String]], body: nil, timeoutMs: 5000
    ))
    let body = String(decoding: Data(resp.body), as: UTF8.self)
    let kex = body.split(separator: "\n")
        .first { $0.hasPrefix("kex=") }?.dropFirst(4)
    print("kex:", kex ?? "unknown", "alpn:", resp.negotiatedProtocol)
    // kex == "X25519MLKEM768" → post-quantum; "X25519" → classical.
}
```

For production verification use Wireshark with `rvictl` USB tethering — filter `tls.handshake.type == 1` and inspect the `key_share` extension for group `0x11EC`. ClientHello is unencrypted; no decryption needed.

For fleet-level telemetry, query Akamai DataStream 2 (or your edge's TLS observability) for the negotiated named group per request, broken down by client OS and app version.

## 10. SPKI cert pinning — how to compute hashes

`PqcConfig.pinnedCertSha256` takes an array of base64-encoded SHA-256 hashes of a certificate's **Subject Public Key Info** (SPKI). Both standard (`+`/`/`) and URL-safe (`-`/`_`) alphabets are accepted, with or without padding. Empty array disables pinning.

A pin matches if **any certificate in the server's chain — leaf or intermediate — has a matching SPKI hash** (the leaf must still parse). This mirrors OkHttp's `CertificatePinner`, Apple's `NSPinnedDomains`, and Android's `NetworkSecurityConfig`.

Compute a SPKI hash. The chain (leaf first, then intermediates) is shown by `-showcerts`:

```sh
# Leaf SPKI:
openssl s_client -servername api.example.com -connect api.example.com:443 < /dev/null 2>/dev/null \
  | openssl x509 -pubkey -noout \
  | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary \
  | base64

# Intermediate SPKI: list the full chain, then run the same pipe on the
# intermediate cert block (the 2nd certificate):
openssl s_client -showcerts -servername api.example.com -connect api.example.com:443 < /dev/null
```

**Recommended: pin your issuing intermediate CA.** Its key has a multi-year lifespan and is far more specific than a public root, so the leaf can rotate freely (CA-forced reissue, ACME renewal) without an app update. Pinning the leaf alone is the most fragile option — a single reissue without a matching pin already shipped will brick the app.

**Always configure at least two pins** (e.g. the current intermediate + a backup intermediate or a pre-deployed next leaf), and document a rotation playbook. **Never pin a public root** (e.g. ISRG Root X1): every cert that root issues would satisfy the pin, defeating the guarantee.

The verifier layers SPKI pinning **on top of** the system trust verification — both must pass. If either fails, the handshake is rejected with `PqcError.pinningFailure`.

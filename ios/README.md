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

After `./scripts/build-ios.sh` at the repo root:

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
pod 'PqcCore', '~> 0.2.0'
```

`pod install` resolves through Trunk, downloads `PqcCore-X.Y.Z.zip` (XCFramework + Swift bindings) from the matching GitHub Release, and wires it in. No local build of this repo required.

Alternative (no Trunk dependency) — pin directly to the raw podspec URL at a release tag:

```ruby
pod 'PqcCore', :podspec => 'https://raw.githubusercontent.com/sriharsha-y/pqc-mobile-client/v0.2.0/PqcCore.podspec'
```

Useful when the consumer's CocoaPods setup can't reach Trunk (corporate firewalls, custom mirrors), or to pin to a specific tag that hasn't been Trunk-pushed yet.

### Swift Package Manager (recommended for native iOS apps)

In your app's `Package.swift`:

```swift
dependencies: [
    .package(url: "https://github.com/sriharsha-y/pqc-mobile-client.git", from: "0.2.0")
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

Behind the scenes: SPM resolves `from: "0.2.0"` to the `v0.2.0` git tag, which points at the `swiftpm` branch's `Package.swift`. That manifest declares `PqcCore.xcframework` as a `binaryTarget` whose URL fetches the matching release asset (`PqcCore-0.2.0.zip`) — same artifact CocoaPods consumes. SPM verifies the SHA256 checksum at download time.

The `swiftpm` branch is auto-maintained by the release workflow. Do not consume `main` directly via SPM — `main` has no `Package.swift` at root, only the Rust crate sources.

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

    static let client: PqcHttpClient = {
        PqcHttpClient(config: PqcConfig(
            pinnedCertSha256: CertPins.spkiSha256,
            enablePostQuantum: true,
            enableHttp3: false,
            defaultTimeoutMs: 15_000
        ))
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
                let pqcResp = try await Self.client.request(req: pqcReq)
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
let pqc = PqcHttpClient(config: PqcConfig(
    pinnedCertSha256: [],
    enablePostQuantum: true,
    enableHttp3: false,
    defaultTimeoutMs: 10_000
))

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

Debug-build sanity check:

```swift
Task {
    let resp = try await PqcURLProtocol.client.request(req: HttpRequest(
        method: .get,
        url: "https://pq.cloudflareresearch.com/",
        headers: [:] as [String: [String]], body: nil, timeoutMs: 5000
    ))
    print("negotiated group:", resp.negotiatedNamedGroup)
}
```

For production verification use Wireshark with `rvictl` USB tethering — filter `tls.handshake.type == 1` and inspect the `key_share` extension for group `0x11EC`. ClientHello is unencrypted; no decryption needed.

For fleet-level telemetry, query Akamai DataStream 2 for the negotiated named group per request, broken down by client OS and app version.

## 10. SPKI cert pinning — how to compute hashes

`PqcConfig.pinnedCertSha256` takes an array of base64-encoded SHA-256 hashes of the **Subject Public Key Info** (SPKI). Empty array disables pinning.

Compute from a live server:

```sh
openssl s_client -servername api.example.com -connect api.example.com:443 < /dev/null 2>/dev/null \
  | openssl x509 -pubkey -noout \
  | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary \
  | base64
```

**Always pin at least two hashes** — the current leaf SPKI and a pre-deployed next leaf SPKI for rotation. Document a rotation playbook for cert renewal.

**Pin leaf SPKIs only.** The verifier enforces **leaf-strict pinning**: only the end-entity (leaf) certificate's SPKI is compared against the pin list, regardless of what the server includes in its chain. Pinning to an intermediate or root CA SPKI will NOT match. This is deliberate — pinning anything other than the leaf (e.g., a popular root like ISRG Root X1) lets any cert under that root pass, defeating the pinning guarantee. For rotation, configure both the active leaf SPKI AND the pre-deployed next leaf SPKI.

The verifier layers SPKI pinning **on top of** the system trust verification — both must pass. If either fails, the handshake is rejected with `PqcError.pinningFailure`.

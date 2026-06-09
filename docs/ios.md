# iOS consumption guide

`pqc_client` on iOS, consumed from:

- **A native iOS app** using `URLSession` (Sections 3 and 4)
- **A native iOS app** using a custom HTTP client or no HTTP framework (Section 5)
- **A React Native iOS app** (Section 6)

The Rust core, the XCFramework, and the generated Swift bindings are the same regardless of consumer.

## 1. Build outputs

> Regenerating bindings manually requires `--features cli` — the `uniffi-bindgen` binary is gated behind it so its deps (clap, goblin, uniffi_bindgen) stay out of the mobile archive.

After `make ios` at the repo root:

```
generated/
├── PqcCore.xcframework
│   ├── ios-arm64/libpqc_client.a                      # ~68 MiB (--features cache)
│   └── ios-arm64_x86_64-simulator/libpqc_client.a     # ~140 MiB (dev only)
└── swift/
    ├── pqc.swift               (UniFFI-generated Swift bindings)
    ├── pqcFFI.h
    └── module.modulemap
```

The `.a` size is not the shipped cost — `clang -dead_strip` + LTO discard most of it. Measured link-time delta to the app binary:

| Build | Static archive (.a) | Linked binary delta in `.app` |
|---|---|---|
| `--features cache` (release default) | ~68 MiB | **~6.0 MiB** |
| no cache (`PQC_CARGO_FEATURES=""`) | ~59 MiB | ~5.0 MiB |

The simulator slice is dev-only and never ships.

## 2. Packaging

### CocoaPods (recommended for RN apps; works for native)

The pod is published to the CocoaPods Trunk registry on every release. In the consumer's `Podfile`:

```ruby
pod 'PqcCore', '~> 0.8.2' # x-release-please-version
```

`pod install` resolves through Trunk, downloads `PqcCore-X.Y.Z.zip` (XCFramework + Swift bindings) from the matching GitHub Release, and wires it in. No local build of this repo required.

Alternative (no Trunk dependency) — pin directly to the raw podspec URL at a release tag:

```ruby
pod 'PqcCore', :podspec => 'https://raw.githubusercontent.com/sriharsha-y/pqc-mobile-client/v0.8.2/PqcCore.podspec' # x-release-please-version
```

Useful when the consumer's CocoaPods setup can't reach Trunk (corporate firewalls, custom mirrors), or to pin to a specific tag that hasn't been Trunk-pushed yet.

### Swift Package Manager (recommended for native iOS apps)

In your app's `Package.swift`:

```swift
dependencies: [
    .package(url: "https://github.com/sriharsha-y/pqc-mobile-client.git", from: "0.8.2") // x-release-please-version
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

SPM verifies the release asset's SHA256 at download time. CocoaPods does not — integrity there relies on HTTPS + GitHub release controls. Prefer SPM (or vendor the XCFramework) if you need byte-level guarantees.

### Apple framework dependencies

The vendored static archive references `Security` (rustls-platform-verifier) and `SystemConfiguration` (hickory-resolver via the `system-configuration` crate). Static `.a` files don't carry `LC_LINKER_OPTION` like dylibs, so each packaging path declares them explicitly — CocoaPods and SPM do this for you:

- **CocoaPods**: `s.frameworks = 'Security', 'SystemConfiguration'` in `PqcCore.podspec`.
- **SPM**: `linkerSettings: [.linkedFramework("Security"), .linkedFramework("SystemConfiguration")]` in `Package.swift`.
- **Manual XCFramework / tarball**: add `-framework Security -framework SystemConfiguration` to **Other Linker Flags**, or both under **Link Binary With Libraries**. Without this, expect `Undefined symbol: _kSCNetworkInterfaceTypeWWAN` (or similar) at link time.

## 3. Native iOS — `URLSession` via `URLProtocol` (drop-in)

`URLProtocol` is the iOS hook. `PqcCore` ships an `open` base class `PqcURLProtocol` that contains the request/response plumbing and a `PqcHttpClient` whose defaults match `URLSessionConfiguration.default` (60 s request timeout, 10 s connect, cookies on, RFC 9111 cache on with a 20 MiB cap in `.cachesDirectory/pqc-http`, 20-redirect limit). Subclass it to customise just the knobs you care about; the rest of the app keeps using `URLSession` unchanged.

```swift
import Foundation
import PqcCore

final class AppPqcURLProtocol: PqcURLProtocol {
    static let pqcHosts: Set<String> = [
        "api.example.com",
        "auth.example.com",
    ]

    /// Override `makeConfig` to set pins / app-specific defaults. The base
    /// class lazily builds one `PqcHttpClient` per subclass under an NSLock.
    override class func makeConfig() -> PqcConfig {
        return .platformDefault(
            pinnedCertSha256: CertPins.spkiSha256,    // see §10
            defaultTimeoutMs: 15_000,
            enableCookies: false,                     // banking: no auto cookie jar
            userAgent: "MyApp/1.0",                   // identify to bank WAF / Akamai
            redirectPolicy: .sameOriginOnly,          // refuse cross-origin 3xx
            enableCache: false                        // opt out of the Rust cache too
        )
    }

    /// Override `shouldHandle` for host gating. Default: every HTTPS request.
    override class func shouldHandle(_ request: URLRequest) -> Bool {
        guard super.shouldHandle(request) else { return false }
        guard let host = request.url?.host else { return false }
        return pqcHosts.contains(host)
    }
}
```

That is the entire URLProtocol — no `startLoading`/`stopLoading`/body-drain/method-mapping/ALPN code to maintain. The base class also:

- strips any inbound `Cookie:` header so the Rust client's jar is the only source of truth (URLSession's behaviour with custom URLProtocols is undocumented; this makes it deterministic);
- emits `cacheStoragePolicy: .notAllowed` so `URLCache` cannot participate (Rust client's RFC 9111 cache is the single cache);
- maps `PqcError.PinningFailure`/`TrustVerification` to `URLError.serverCertificateUntrusted`, `Timeout` to `URLError.timedOut`, etc. — override `mapError(_:)` to customise.

### Registration — native app

Two paths depending on which session the app uses.

**Pattern A — `URLSession.shared`:**

```swift
if #unavailable(iOS 26, *) {
    URLProtocol.registerClass(AppPqcURLProtocol.self)
}
// iOS 26+ negotiates X25519MLKEM768 natively. If you ALSO want SPKI
// pinning on iOS 26+, drop the @unavailable and register unconditionally.
```

**Pattern B — a custom `URLSession`:** use the bundled `registerIfNeeded(into:)` helper, which inserts at index 0 of `protocolClasses` on pre-iOS-26 and no-ops on iOS 26+. (Use `register(into:)` instead if you want to register unconditionally — e.g. to keep SPKI pinning on iOS 26+.)

```swift
let cfg = URLSessionConfiguration.default
AppPqcURLProtocol.registerIfNeeded(into: cfg)
let session = URLSession(configuration: cfg)
```

Existing API code (Alamofire, Moya, raw `URLSession.dataTask`) using this session continues to work unchanged.

### Cookies and response cache — Rust-owned

The base class deliberately does not bridge `HTTPCookieStorage.shared` or `URLCache.shared` because the wrapper boundary makes both unreliable:

- `URLCache` is bypassed by the documented `cacheStoragePolicy: .notAllowed` contract. The Rust client's RFC 9111 cache is the only cache. By default (`PqcConfig.platformDefault()`) it is on, with a 20 MiB disk tier in `.cachesDirectory/pqc-http`.
- `HTTPCookieStorage.shared` would suffer from `HTTPURLResponse`'s `[String: String]` backing — multiple `Set-Cookie` headers comma-join, and `Expires=Wed, 21 Oct 2026 …` corrupts the join. The Rust client's own cookie jar (`enableCookies: true` by default) handles this correctly and persists state across requests routed through the URLProtocol.

**Documented constraint:** cookies set via the PQC flow are *not* visible to plain `URLSession.shared` calls elsewhere in the app, and vice versa. For most apps this isolation is desirable; apps that need a bridge would have to write their own `URLProtocol` from scratch (the base class's `emit` / `buildRequest` are file-private to keep the cookie/cache invariants enforced).

See `examples/RnSample/ios/RnSample/PqcURLProtocol.swift` for a working subclass.

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

For new code paths or non-URLSession-based clients, call `PqcHttpClient` directly. The UniFFI-generated Swift class has Swift-native `async`/`throws`. `PqcConfig.platformDefault(...)` gives you URLSession-aligned defaults so you only have to specify what's different.

```swift
let pqc = try PqcHttpClient(
    config: .platformDefault(
        pinnedCertSha256: CertPins.spkiSha256,
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
    // resp is `PqcResponse` — streaming-first like URLSession.bytes(for:).
    // `bytes()` is the buffered convenience for small JSON; for large
    // downloads loop on `readChunk()` to keep memory bounded.
    return Data(try await resp.bytes())
}
```

### Streaming a large download

`PqcResponse.readChunk()` returns the next chunk or `nil` at EOF. Mirrors OkHttp `ResponseBody.source()` and `URLSession.bytes(for:)`. Headers/status are available before the first chunk arrives, so you can decide whether to drain or abort based on `resp.headers()` / `resp.status()`.

```swift
let resp = try await pqc.request(req: req)
guard resp.status() == 200 else { throw MyError.badStatus(resp.status()) }

let out = FileHandle(forWritingAtPath: "/path/to/output")!
defer { try? out.close() }

while let chunk = try await resp.readChunk() {
    try out.write(contentsOf: chunk)
}
```

### Cancellation

UniFFI 0.29 does **not** propagate Swift `Task.cancel()` into Rust. To abort an in-flight body read, call `resp.cancel()` explicitly. Idempotent.

```swift
let task = Task {
    let resp = try await pqc.request(req: req)
    // ... read chunks ...
}

// Some time later, the user backs out of the view:
resp.cancel()               // sync — releases the underlying connection
task.cancel()               // cancels the Swift Task (Rust side already aborted)
```

Dropping a `PqcResponse` without calling `cancel()` or draining via `bytes()`/`readChunk()`-to-EOF also aborts the body — the connection returns to the pool when the response is deallocated. The explicit `cancel()` exists so you can release the connection promptly without waiting for ARC.

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
            AppPqcURLProtocol.registerIfNeeded(into: cfg)
            return cfg
        }
        return super.application(application, didFinishLaunchingWithOptions: launchOptions)
    }
}
```

The `AppPqcURLProtocol` class is identical to the native case (Section 3).

### Wiring from `AppDelegate.mm` (Objective-C++)

If your RN template uses the Objective-C++ AppDelegate (`.mm`) instead of Swift, the same `RCTSetCustomNSURLSessionConfigurationProvider` call works — but ObjC++ can only see Swift symbols marked `@objc`, and import order matters:

```objc++
// AppDelegate.mm

// PqcCore-Swift.h first (declares PqcURLProtocol — superclass of
// AppPqcURLProtocol). Replace "MyApp" with your product module name.
#import "PqcCore-Swift.h"
#import "MyApp-Swift.h"

// ... inside didFinishLaunchingWithOptions: ...
RCTSetCustomNSURLSessionConfigurationProvider(^NSURLSessionConfiguration *{
    NSURLSessionConfiguration *cfg = [NSURLSessionConfiguration defaultSessionConfiguration];
    [AppPqcURLProtocol registerIfNeededInto:cfg];   // ← @objc selector form
    return cfg;
});
```

Your Swift bridge subclass:

```swift
// MyApp/AppPqcURLProtocol.swift
import PqcCore

@objc(AppPqcURLProtocol)
public class AppPqcURLProtocol: PqcURLProtocol {
    public override class func makeConfig() -> PqcConfig {
        return .platformDefault(
            pinnedCertSha256: CertPins.spkiSha256,
            userAgent: "MyApp/1.0",
            redirectPolicy: .sameOriginOnly
        )
    }
}
```

The selector `registerIfNeededInto:` is Swift's default mapping of `registerIfNeeded(into:)`; `register(into:)` maps to `registerInto:`.

> **If your Podfile uses `use_frameworks!`** (any linkage), swap the quote-form import for `#import <PqcCore/PqcCore-Swift.h>` (or `@import PqcCore;`). The quote-form above is for the default static-libs packaging and relies on the podspec's `user_target_xcconfig`; under `use_frameworks!` the header lives inside `PqcCore.framework/Headers/` and CocoaPods adds the right search path automatically.

> **Available since 0.8.1.** Older releases don't `@objc`-annotate the static helpers or set `user_target_xcconfig`. On 0.8.0, either upgrade or add a thin Swift `@objc` wrapper on your subclass.

### Direct use of `PqcHttpClient` from Objective-C++

The UniFFI-generated classes (`PqcHttpClient`, `PqcResponse`, `PqcConfig`, `BodyProvider`) are pure Swift and are not bridged to Objective-C. To call them from `.mm`, write a small Swift `@objc` wrapper and call that. The URLProtocol path above is the recommended integration; direct use is only needed for non-`URLSession` protocols (e.g. WebSocket-over-HTTPS, gRPC).

## 7. iOS 26 gate

Covered in §3 — the bundled `registerIfNeeded(into:)` helper no-ops on iOS 26+ where `URLSession` already negotiates `X25519MLKEM768` natively.

## 8. Export compliance

Bundling Rust crypto promotes the app from "uses-OS-encryption-only" (exempt) to "uses non-exempt encryption":

- `Info.plist`: `ITSAppUsesNonExemptEncryption = YES`
- File the annual self-classification report (ERN) with U.S. BIS — see [Apple's guide](https://developer.apple.com/documentation/security/complying-with-encryption-export-regulations).
- ML-KEM is FIPS 203 and qualifies for the standard TLS export exemption (no CCATS needed).
- Apache-2.0 attribution for `aws-lc-rs`, `rustls`, `reqwest` in the app's acknowledgements.

## 9. Verification

Debug-build sanity check. `PqcResponse` deliberately does not expose the negotiated key-exchange group (it is a per-connection property the client can only observe via a racy process-global). Confirm it from the **server's** report instead: Cloudflare's `/cdn-cgi/trace` returns a `kex=` line.

```swift
Task {
    let client = try PqcHttpClient(config: PqcConfig(/* … */))
    let resp = try await client.request(req: HttpRequest(
        method: .get,
        url: "https://pq.cloudflareresearch.com/cdn-cgi/trace",
        headers: [:] as [String: [String]], body: nil, timeoutMs: 5000
    ))
    let body = String(decoding: Data(try await resp.bytes()), as: UTF8.self)
    let kex = body.split(separator: "\n")
        .first { $0.hasPrefix("kex=") }?.dropFirst(4)
    print("kex:", kex ?? "unknown", "alpn:", resp.negotiatedProtocol())
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

## 11. Response caching (opt-in)

The Rust core can cache HTTP responses (RFC 9111), so repeat GETs are served without a network round-trip — the same idea as `URLCache`, but it lives in the core because the core owns the socket (the `URLProtocol` path marks its responses `.notAllowed`, see §3). On iOS it's a **memory + disk** cache, mirroring `URLCache`'s composite; on Android it's disk-only. It is **off by default**. Enable it per client:

```swift
let caches = FileManager.default
    .urls(for: .cachesDirectory, in: .userDomainMask).first!
let httpCacheDir = caches.appendingPathComponent("pqc-http").path

let config = PqcConfig(
    // … existing fields …
    redirectPolicy: .sameOriginOnly,
    enableCache: true,
    cacheDir: httpCacheDir,             // persistent disk tier; nil → memory-only
    maxCacheBytes: 20 * 1024 * 1024     // 20 MiB, like URLCache's disk capacity; nil → 20 MiB
)

// On logout / session end (and a "Clear cache" button):
await client.clearCache()
let bytes = await client.cacheSizeBytes()   // UInt64, e.g. for "Clear cache (1.2 MB)"
```

**Use exactly one cache.** Keep the `URLProtocol`'s `cacheStoragePolicy: .notAllowed` (and leave `URLCache` unconfigured for the routed session) so the core's cache is the single source of truth — no double storage. Direct-`URLSession`/direct-API consumers just set the config above; nothing else changes.

### What gets cached

Cacheability is decided by method + status + cache headers — not by extension or `Content-Type`. This is a **private** cache (`shared = false`), so it will cache `Authorization`-bearing responses when their headers permit (same as `URLCache`/OkHttp). Use `Cache-Control: no-store` server-side to keep sensitive endpoints out; `clearCache()` on logout is the backstop.

### Notes / behavior vs. native

- **Builds:** only effective in artifacts built with the `cache` cargo feature (the official release builds enable it). In a feature-less build, `enableCache: true` makes the initializer throw `PqcError.invalidRequest`, and `clearCache`/`cacheSizeBytes` are inert.
- **vs. `URLCache`:** the memory tier is true LRU; the disk tier evicts oldest-first (FIFO) once `maxCacheBytes` is exceeded. Like `URLCache`, we apply a **per-entry cap of ~5% of total capacity** — with a 20 MiB cache, individual responses larger than ~1 MiB skip the cache, so one large download can't evict the entire hot set. We deliberately do **not** replicate `URLCache`'s 200–299-only status filter (we cache the broader RFC set).
- **Security:** a cache *hit* serves bytes without a TLS handshake, so the PQC / pinning guarantees re-apply only on a miss or revalidation. That's expected and matches every HTTP cache.

## 12. DNS resolver — `dnsResolver` (opt-in)

By default the client uses libc `getaddrinfo` driven by the iOS system resolver chain. Most apps want this — leave `dnsResolver` unset.

Set `dnsResolver = .hickory` to switch to the bundled `hickory-dns` async resolver. This enables **RFC 8305 Happy Eyeballs** — concurrent IPv4/IPv6 connection racing, materially faster on dual-stack networks where one address family is broken (common on some cellular carriers). The trade-off: hickory uses its own DNS path; if your app relies on iOS-managed DNS configuration (e.g. profile-installed resolvers), leave the resolver at the default `.system`.

```swift
let config = PqcConfig.platformDefault(
    // ...
    dnsResolver: .hickory  // opt-in for Happy Eyeballs
)
```

## 13. Streaming upload bodies — `BodyProvider` (large file uploads)

The default upload path inlines the body via `HttpRequest.body: Data` (or `URLRequest.httpBody`), buffering the entire payload in memory. For large uploads (photos, videos, multipart with file parts) use the streaming path: set `URLRequest.httpBodyStream` and the `PqcURLProtocol` wrapper automatically bridges it through `BodyProvider`, streaming chunk-by-chunk to the network. **Peak memory tracks one chunk (~64 KiB)**, not the file size — matches `URLSession.uploadTask(withStreamedRequest:)` semantics.

For consumers calling `PqcHttpClient` directly (bypassing `URLProtocol`), implement `BodyProvider` in Swift and set `HttpRequest.bodyStream`:

```swift
final class FileBodyProvider: BodyProvider {
    private let stream: InputStream
    private var opened = false
    private var closed = false
    private let lock = NSLock()

    init(fileURL: URL) {
        self.stream = InputStream(url: fileURL)!
    }

    func nextChunk() throws -> Data? {
        lock.lock(); defer { lock.unlock() }
        if closed { return nil }
        if !opened { stream.open(); opened = true }
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        let n = stream.read(&buf, maxLength: buf.count)
        if n < 0 { throw PqcError.invalidRequest(message: "read failed") }
        if n == 0 { stream.close(); closed = true; return nil }
        return Data(bytes: buf, count: n)
    }

    func cancel() {
        // Idempotent — Rust calls this on upload abort to release the fd.
        lock.lock(); defer { lock.unlock() }
        if opened && !closed { stream.close() }
        closed = true
    }
}

let fileURL = URL(fileURLWithPath: "/path/to/big.bin")
let resp = try await pqc.request(req: HttpRequest(
    method: .post,
    url: "https://api.example.com/upload",
    headers: ["Content-Type": ["application/octet-stream"]],
    body: nil,                                       // ← mutually exclusive
    bodyStream: FileBodyProvider(fileURL: fileURL),  // ← stream
    bodyStreamLength: UInt64(fileSize),              // optional Content-Length;
                                                     //   nil → chunked encoding
    timeoutMs: nil
))
```

`nextChunk()` is invoked from Rust via tokio `spawn_blocking`, so blocking reads (file I/O, `InputStream.read`) are safe. `cancel()` is called when the upload aborts (network error, caller dropped the request, server closed mid-stream) — implement it to release fds and other resources. **Streaming bodies are not retry-safe** — once consumed, they can't be replayed; construct a fresh `BodyProvider` if you need to retry.

## 14. Tuning knobs

Beyond the basics in §3, `PqcConfig` exposes the following knobs (all optional, set on the config you return from `makeConfig()`):

| Field | Default | Notes |
|---|---|---|
| `readIdleTimeoutMs` | `nil` | Per-read idle timeout — kills a stalled stream without burning the total `defaultTimeoutMs` budget. Mirrors OkHttp's `readTimeout`. Recommended: 10–30 s for APIs, 60 s+ for large file downloads. |
| `maxInflightTotal` | `Some(64)` | Global concurrent-request cap. `nil` disables. Matches OkHttp `Dispatcher.maxRequests`. |
| `maxInflightPerHost` | `Some(5)` | Per-host concurrent-request cap. `nil` disables. Matches OkHttp `Dispatcher.maxRequestsPerHost`; URLSession's analogous cap is 6. |
| `maxMemoryCacheBytes` | `nil` (= 4 MiB) | In-memory LRU tier for the response cache, on top of the disk tier. Matches `URLCache`'s memory tier. `Some(0)` opts out entirely. |
| `dnsResolver` | `nil` (= `.system`) | See §12. |

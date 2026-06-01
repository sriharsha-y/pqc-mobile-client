import Foundation
#if canImport(UIKit)
import UIKit
#endif

public extension PqcConfig {

    /// Builds a `PqcConfig` whose defaults align with
    /// `URLSessionConfiguration.default`: cookies on, RFC 9111 cache on
    /// (20 MiB in `<.cachesDirectory>/pqc-http`), 60 s request / 10 s
    /// connect timeouts, follow up to 20 redirects. URLSession does not
    /// surface a TCP-connect timeout, so we pick 10 s for parity with
    /// Android / OkHttp. Verified against `swift-corelibs-foundation`'s
    /// `URLSessionConfiguration.init(correctly:)`. Safe to call from any
    /// thread.
    ///
    /// The parameter list mirrors the 9 fields of `PqcConfig`. When a field
    /// is added to the Rust struct, the `PqcConfig(...)` call below stops
    /// compiling — that compile error is the upgrade signal.
    static func platformDefault(
        pinnedCertSha256: [String] = [],
        defaultTimeoutMs: UInt64? = 60_000,
        connectTimeoutMs: UInt64? = 10_000,
        enableCookies: Bool = true,
        userAgent: String? = nil,
        redirectPolicy: RedirectPolicy = .limited(max: 20),
        enableCache: Bool = true,
        cacheDir: String? = nil,
        maxCacheBytes: UInt64? = 20 * 1024 * 1024
    ) -> PqcConfig {
        return PqcConfig(
            pinnedCertSha256: pinnedCertSha256,
            defaultTimeoutMs: defaultTimeoutMs,
            connectTimeoutMs: connectTimeoutMs,
            enableCookies: enableCookies,
            userAgent: userAgent ?? PqcConfig.defaultIOSUserAgent(),
            redirectPolicy: redirectPolicy,
            enableCache: enableCache,
            cacheDir: cacheDir ?? PqcConfig.defaultCacheDirectory(),
            maxCacheBytes: maxCacheBytes
        )
    }

    /// Best-effort `"<CFBundleName>/<short-version> (<model>; iOS <ver>; CFNetwork)"`.
    /// reqwest's default UA gets flagged by many WAFs (Akamai, bank allowlists),
    /// so we always send something recognisable when the caller passes nil.
    static func defaultIOSUserAgent() -> String {
        let bundle = Bundle.main
        let name = (bundle.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? (bundle.object(forInfoDictionaryKey: "CFBundleExecutable") as? String)
            ?? "PqcCore"
        let version = (bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
            ?? "0"
        #if canImport(UIKit)
        let model = UIDevice.current.model
        let system = UIDevice.current.systemVersion
        return "\(name)/\(version) (\(model); iOS \(system); CFNetwork)"
        #else
        return "\(name)/\(version) (iOS; CFNetwork)"
        #endif
    }

    /// `<.cachesDirectory>/pqc-http`; nil if FileManager can't resolve
    /// (treated as "disable disk tier" by the Rust client).
    static func defaultCacheDirectory() -> String? {
        guard let base = FileManager.default
            .urls(for: .cachesDirectory, in: .userDomainMask).first
        else { return nil }
        return base.appendingPathComponent("pqc-http", isDirectory: true).path
    }
}

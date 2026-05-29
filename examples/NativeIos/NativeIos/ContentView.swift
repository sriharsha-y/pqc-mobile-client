import SwiftUI
import UIKit
import Foundation

// NOTE: there is no `import PqcCore` here. This sample compiles the generated
// UniFFI Swift bindings (../../generated/swift/pqc.swift) directly into the
// app target, so `PqcHttpClient`, `PqcConfig`, etc. are in *this* module.
// (When you instead consume the published SwiftPM/CocoaPods product, the
// bindings live in the `PqcCore` module and you DO `import PqcCore`.)

private let traceUrl = "https://pq.cloudflareresearch.com/cdn-cgi/trace"

/// Mirrors the React Native sample's dark card palette so the three samples
/// look like one product.
private enum Palette {
    static let bg        = Color(hex: 0x0B0D11)
    static let card      = Color(hex: 0x161A22)
    static let title     = Color(hex: 0xE7EAF0)
    static let accent    = Color(hex: 0x5D97F7)
    static let muted     = Color(hex: 0x7D8595)
    static let kexPqc    = Color(hex: 0x5DD193)
    static let kexClass  = Color(hex: 0xE8B94C)
    static let error     = Color(hex: 0xFF6F6F)
}

private enum FetchState {
    case idle
    case loading
    case ok(status: UInt16, kex: String?, alpn: String)
    case error(String)
}

struct ContentView: View {
    @State private var pqcEnabled = true
    @State private var state: FetchState = .idle

    private var isLoading: Bool {
        if case .loading = state { return true }
        return false
    }

    var body: some View {
        ZStack {
            Palette.bg.ignoresSafeArea()

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text("pqc-mobile-client")
                        .font(.system(size: 22, weight: .semibold))
                        .foregroundColor(Palette.title)
                    Text("Platform: iOS \(UIDevice.current.systemVersion)")
                        .font(.system(size: 13))
                        .foregroundColor(Palette.accent)
                        .padding(.bottom, 6)

                    toggleCard
                    resultCard
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .preferredColorScheme(.dark)
        .task { await run(pqc: pqcEnabled) }
    }

    // MARK: - Toggle card

    private var toggleCard: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Advertise post-quantum")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundColor(Palette.title)
                Text(pqcEnabled ? "X25519MLKEM768 offered"
                                : "disabled (classical only)")
                    .font(.system(size: 12))
                    .foregroundColor(Palette.muted)
            }
            Spacer()
            Toggle("", isOn: $pqcEnabled)
                .labelsHidden()
                .tint(Palette.accent)
                .disabled(isLoading)
                .onChange(of: pqcEnabled) { newValue in
                    Task { await run(pqc: newValue) }
                }
        }
        .padding(16)
        .background(Palette.card)
        .cornerRadius(14)
    }

    // MARK: - Result card

    private var resultCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Cloudflare /cdn-cgi/trace")
                .font(.system(size: 16, weight: .semibold))
                .foregroundColor(Palette.title)
            Text(traceUrl)
                .font(.system(size: 12))
                .foregroundColor(Palette.muted)
                .padding(.top, 2)
                .padding(.bottom, 10)

            switch state {
            case .idle:
                Text("idle").font(.system(size: 13)).foregroundColor(Palette.muted)

            case .loading:
                HStack(spacing: 8) {
                    ProgressView()
                    Text("Performing TLS handshake…")
                        .font(.system(size: 13)).foregroundColor(Palette.muted)
                }

            case let .ok(status, kex, alpn):
                fieldLabel("Negotiated KEX (server-reported)")
                if let kex {
                    let pqc = kex.uppercased().contains("MLKEM")
                    Text(kex + (pqc ? "  ✓ post-quantum" : "  (classical)"))
                        .font(.system(size: 16, design: .monospaced))
                        .foregroundColor(pqc ? Palette.kexPqc : Palette.kexClass)
                    caption(pqc
                        ? "PQC offered and negotiated — confirmed by the edge."
                        : "PQC disabled on the client — classical handshake as expected.")
                } else {
                    Text("not reported")
                        .font(.system(size: 16, design: .monospaced))
                        .foregroundColor(Palette.muted)
                }
                fieldLabel("ALPN")
                value(alpn)
                fieldLabel("HTTP status")
                value(String(status))

            case let .error(message):
                fieldLabel("Error")
                Text(message).font(.system(size: 13)).foregroundColor(Palette.error)
                    .padding(.top, 4)
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card)
        .cornerRadius(14)
    }

    private func fieldLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.system(size: 12))
            .foregroundColor(Palette.muted)
            .padding(.top, 12)
    }

    private func value(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 16, design: .monospaced))
            .foregroundColor(Palette.title)
    }

    private func caption(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 12).italic())
            .foregroundColor(Palette.muted)
            .padding(.top, 4)
    }

    // MARK: - The actual PQC request

    @MainActor
    private func run(pqc: Bool) async {
        state = .loading
        do {
            // The constructor throws on malformed config (e.g. a bad pin).
            // `enablePostQuantum` is what the toggle drives: false drops the
            // X25519MLKEM768 hybrid so the ClientHello carries classical
            // groups only, and the edge reports kex=X25519.
            let client = try PqcHttpClient(config: PqcConfig(
                pinnedCertSha256: [],
                enablePostQuantum: pqc,
                defaultTimeoutMs: 15_000,
                connectTimeoutMs: nil,
                maxBodyBytes: nil,
                enableCookies: false,
                userAgent: "PqcNativeIosSample/1.0",
                redirectPolicy: .sameOriginOnly,
                // Opt-in RFC 9111 response cache (off here; the trace endpoint
                // is uncacheable anyway). To enable, set enableCache: true and
                // pass a Caches dir path. See docs/ios.md.
                enableCache: false,
                cacheDir: nil,
                maxCacheBytes: nil
            ))

            let resp = try await client.request(req: HttpRequest(
                method: .get,
                url: traceUrl,
                headers: [:],
                body: nil,
                timeoutMs: 5_000
            ))

            let body = String(decoding: Data(resp.body), as: UTF8.self)
            let kex = body.split(separator: "\n")
                .first { $0.hasPrefix("kex=") }
                .map { String($0.dropFirst(4)) }

            state = .ok(status: resp.status, kex: kex, alpn: resp.negotiatedProtocol)
        } catch {
            state = .error("\(error)")
        }
    }
}

private extension Color {
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}

#Preview {
    ContentView()
}

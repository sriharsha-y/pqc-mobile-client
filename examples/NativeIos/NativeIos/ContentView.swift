import SwiftUI
import Foundation

// NOTE: there is no `import PqcCore` here. This sample compiles the generated
// UniFFI Swift bindings (../../generated/swift/pqc.swift) directly into the
// app target, so `PqcHttpClient`, `PqcConfig`, etc. are in *this* module.
// (When you instead consume the published SwiftPM/CocoaPods product, the
// bindings live in the `PqcCore` module and you DO `import PqcCore`.)

struct ContentView: View {
    @State private var output = "Tap to verify post-quantum TLS.\n\nExpected: kex = X25519MLKEM768"
    @State private var running = false

    var body: some View {
        VStack(spacing: 20) {
            Button(running ? "Running…" : "Verify post-quantum TLS") {
                Task { await verify() }
            }
            .disabled(running)

            ScrollView {
                Text(output)
                    .font(.system(.body, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
        }
        .padding()
    }

    /// Build a client, hit Cloudflare's PQC test endpoint, and read back the
    /// key-exchange group the server saw — the server-authoritative way to
    /// confirm X25519MLKEM768 was negotiated (see docs/ios.md §9).
    @MainActor
    private func verify() async {
        running = true
        defer { running = false }
        output = "Running…"

        do {
            // The constructor throws on malformed config (e.g. a bad pin).
            let client = try PqcHttpClient(config: PqcConfig(
                pinnedCertSha256: [],          // platform trust only
                enablePostQuantum: true,
                defaultTimeoutMs: 15_000,
                connectTimeoutMs: nil,         // 10s default
                maxBodyBytes: nil,             // 16 MiB default
                enableCookies: false,
                userAgent: "PqcNativeIosSample/1.0",
                redirectPolicy: .sameOriginOnly
            ))

            let resp = try await client.request(req: HttpRequest(
                method: .get,
                url: "https://pq.cloudflareresearch.com/cdn-cgi/trace",
                headers: [:],
                body: nil,
                timeoutMs: 5_000
            ))

            let body = String(decoding: Data(resp.body), as: UTF8.self)
            let kex = body.split(separator: "\n")
                .first { $0.hasPrefix("kex=") }
                .map { String($0.dropFirst(4)) } ?? "unknown"

            let verdict = kex == "X25519MLKEM768"
                ? "✅ Post-quantum hybrid negotiated."
                : "⚠️ Classical KEX (\(kex)) — server did not negotiate PQC."

            output = """
            status = \(resp.status)
            alpn   = \(resp.negotiatedProtocol)
            kex    = \(kex)

            \(verdict)
            """
        } catch {
            output = "❌ ERROR: \(error)"
        }
    }
}

#Preview {
    ContentView()
}

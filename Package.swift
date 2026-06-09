// swift-tools-version: 5.7
// AUTO-GENERATED — do not hand-edit `Package.swift` at the repo root.
// Edit this template (`.github/swiftpm-template/Package.swift.in`) and
// changes propagate to `Package.swift` at the repo root via the next
// `publish-swiftpm` workflow run, which also re-points the release tag
// to the resulting commit so SPM consumers using `from: "X.Y.Z"` always
// resolve to a `Package.swift` with matching URL + checksum.

import PackageDescription

let version = "0.8.2"

let package = Package(
    name: "PqcCore",
    platforms: [.iOS(.v13)],
    products: [
        .library(name: "PqcCore", targets: ["PqcCore"])
    ],
    targets: [
        // One release zip (PqcCore-X.Y.Z.zip) serves both SPM and CocoaPods;
        // SPM finds the .xcframework at its root and ignores the siblings.
        //
        // The binaryTarget name MUST match the xcframework modulemap's module
        // name ("pqcFFI"), else consumers hit "no such module pqcFFI".
        .binaryTarget(
            name: "pqcFFI",
            url: "https://github.com/sriharsha-y/pqc-mobile-client/releases/download/v\(version)/PqcCore-\(version).zip",
            checksum: "191cd976d99b041ce6fc504470dd9682450de6415265508145b07f0ff6278b42"
        ),
        // UniFFI-generated Swift binding under Sources/PqcCore, refreshed by
        // `publish-swiftpm` each release. Its `import pqcFFI` matches the
        // xcframework modulemap and the binaryTarget name above.
        //
        // linkerSettings: static .a files don't auto-link Apple frameworks.
        // Security: rustls-platform-verifier. SystemConfiguration:
        // hickory-resolver via the system-configuration crate. Propagates
        // to consumer targets — no manual link step needed.
        .target(
            name: "PqcCore",
            dependencies: ["pqcFFI"],
            path: "Sources/PqcCore",
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
            ]
        )
    ]
)

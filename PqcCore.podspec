Pod::Spec.new do |s|
  s.name             = 'PqcCore'
  s.version          = '0.8.0' # x-release-please-version
  s.summary          = 'Post-Quantum TLS HTTPS client (Rust + UniFFI) — iOS XCFramework + Swift bindings.'
  s.description      = <<-DESC
    Vendors PqcCore.xcframework (rustls + rustls-post-quantum + aws-lc-rs)
    and the UniFFI-generated Swift binding. Consumers do not need a local
    build; the podspec fetches the prebuilt XCFramework + Swift binding from
    the matching GitHub release asset (PqcCore-X.Y.Z.zip).
  DESC
  s.homepage         = 'https://github.com/sriharsha-y/pqc-mobile-client'
  s.license          = { :type => 'Apache-2.0', :file => 'LICENSE' }
  s.author           = { 'Harsha Yarabarla' => 'harsha.yarabarla@gmail.com' }
  # Per-release prebuilt bundle attached to the GitHub release. CocoaPods
  # unpacks the zip into the Pod root — paths below are relative to it.
  # The CI release flow is the sole producer; the layout is contract-locked.
  s.source = {
    :http => "https://github.com/sriharsha-y/pqc-mobile-client/releases/download/v#{s.version}/PqcCore-#{s.version}.zip",
    :type => 'zip',
  }
  s.platform         = :ios, '13.0'
  s.swift_version    = '5.9'

  # Source paths are RELATIVE TO THE POD ROOT. Bare names resolve two ways:
  #   1. Published: the release zip stages pqc.swift + PqcCore.xcframework
  #      at the zip root, so they resolve directly in the unpacked Pod.
  #   2. Local dev (:path => '../../../'): scripts/build-ios.sh creates
  #      symlinks at the repo root into generated/ (both .gitignore'd), so
  #      the same bare names resolve via symlink.
  #
  # preserve_paths defensively keeps CocoaPods from stripping XCFramework
  # slices on :http sources (a 1.10+ edge case for some zip layouts).
  s.source_files      = 'pqc.swift', 'PqcConfig+Defaults.swift', 'PqcURLProtocol.swift'
  s.vendored_frameworks = 'PqcCore.xcframework'
  s.preserve_paths    = 'PqcCore.xcframework'

  # The vendored static archive references symbols from two Apple
  # frameworks. Static .a files don't carry LC_LINKER_OPTION like
  # dylibs, so each must be declared here — otherwise the consumer's
  # app fails at link time with "Undefined symbol: …".
  #
  # - Security: rustls-platform-verifier's SecTrust* / SecKey* calls
  #   (_kSecKeyAlgorithm*).
  # - SystemConfiguration: hickory-resolver (added when the streaming
  #   refactor introduced the opt-in Hickory DNS path) transitively
  #   pulls in the `system-configuration` Rust crate, which references
  #   SCDynamicStoreCreate, SCNetworkReachabilityCreateWithName,
  #   kSCNetworkInterfaceType* constants, etc. These ship in
  #   SystemConfiguration.framework, which Apple does NOT auto-link.
  s.frameworks = 'Security', 'SystemConfiguration'

  # aws-lc-sys's C++ runtime support symbols come from libc++ on Apple
  # platforms. s.libraries is the idiomatic, link-phase-correct form.
  s.libraries = 'c++'

  # Required because this Pod vendors an XCFramework AND ships Swift sources,
  # especially under `use_frameworks!` (RN 0.74+ New Architecture). Without
  # it CocoaPods may build a clang framework whose module map doesn't
  # re-export the XCFramework's `pqcFFI` module, so `import PqcCore` compiles
  # but links with `Undefined symbol: _$s6pqcFFI...`.
  s.static_framework = true
end

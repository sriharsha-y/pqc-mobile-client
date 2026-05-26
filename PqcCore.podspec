Pod::Spec.new do |s|
  s.name             = 'PqcCore'
  s.version          = '0.1.1' # x-release-please-version
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
  # :http source points at the per-release prebuilt bundle attached to
  # the GitHub release. CocoaPods downloads, verifies, and unpacks the
  # zip into the Pod root — paths below are relative to that root.
  # The CI release flow (release.yml: build-ios → Package step) is the
  # sole producer of these zips; the layout is contract-locked.
  s.source = {
    :http => "https://github.com/sriharsha-y/pqc-mobile-client/releases/download/v#{s.version}/PqcCore-#{s.version}.zip",
    :type => 'zip',
  }
  s.platform         = :ios, '13.0'
  s.swift_version    = '5.9'

  s.source_files      = 'pqc.swift'
  s.vendored_frameworks = 'PqcCore.xcframework'

  # The vendored static archive references Security.framework symbols
  # (rustls-platform-verifier's SecTrust* / SecKey* / SecCertificate*
  # calls). Static .a files don't carry LC_LINKER_OPTION the way dylibs
  # do, so without this declaration the consumer's project fails to
  # link with "Undefined symbol: _kSecKeyAlgorithm...".
  #
  # Verified by `nm -u` on both ios-arm64 and ios-arm64_x86_64-simulator
  # slices: the only Apple-framework symbol prefixes referenced are
  # _kSec* (Security) and _CCR* (CommonCrypto, which is in libSystem
  # and auto-linked — no framework declaration needed). CFNetwork,
  # SystemConfiguration, and Foundation are NOT referenced.
  s.frameworks = 'Security'

  # libc++ is needed because aws-lc-sys's C++ runtime support symbols
  # come from libc++ on Apple platforms. s.libraries is the idiomatic
  # form; CocoaPods aggregates it across pods and applies it to the
  # right link phase.
  s.libraries = 'c++'

  # static_framework = true is required when a Pod both vendors an
  # XCFramework AND ships Swift sources, especially under
  # `use_frameworks!` (common in RN 0.74+ New Architecture). Without it
  # CocoaPods may build PqcCore as a clang framework whose auto-generated
  # module map doesn't re-export the vendored XCFramework's `pqcFFI`
  # module — `import PqcCore` would succeed at compile but link with
  # `Undefined symbol: _$s6pqcFFI...` errors. Setting this explicitly
  # keeps the integration working across both static-lib and framework
  # link modes.
  s.static_framework = true
end

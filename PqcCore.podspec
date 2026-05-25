Pod::Spec.new do |s|
  s.name             = 'PqcCore'
  s.version          = '0.1.0'
  s.summary          = 'Post-Quantum TLS HTTPS client (Rust + UniFFI) — iOS XCFramework + Swift bindings.'
  s.description      = <<-DESC
    Vendors PqcCore.xcframework (rustls + rustls-post-quantum + aws-lc-rs)
    and the UniFFI-generated Swift binding. Run ./scripts/build-ios.sh in
    this repo first to produce the XCFramework at generated/PqcCore.xcframework.
  DESC
  s.homepage         = 'https://github.com/sriharsha-y/pqc-mobile-client'
  s.license          = { :type => 'Apache-2.0' }
  s.author           = { 'Harsha Yarabarla' => 'harsha.yarabarla@gmail.com' }
  s.source           = { :git => 'https://github.com/sriharsha-y/pqc-mobile-client.git', :tag => s.version.to_s }
  s.platform         = :ios, '15.1'
  s.swift_version    = '5.9'

  s.source_files      = 'generated/swift/pqc.swift'
  s.vendored_frameworks = 'generated/PqcCore.xcframework'

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

  s.pod_target_xcconfig = {
    'OTHER_LDFLAGS' => '-lc++',
  }
end

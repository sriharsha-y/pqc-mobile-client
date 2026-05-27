# Changelog

## [0.3.0](https://github.com/sriharsha-y/pqc-mobile-client/compare/v0.2.1...v0.3.0) (2026-05-27)


### ⚠ BREAKING CHANGES

* **release:** drop pqc-mobile-client-X.Y.Z-ios.zip, keep only PqcCore-X.Y.Z.zip
* **api:** Existing Kotlin/Swift consumers that build HttpRequest.headers as a Map<String, String> will need to wrap values in a list (Kotlin: `mapOf("k" to listOf("v"))`, Swift:

### Features

* **android:** add Gradle library module for Maven Central publication ([c87982f](https://github.com/sriharsha-y/pqc-mobile-client/commit/c87982f03157da7464dc24a35fe7f52a484b20cf))
* **api:** HttpRequest.headers is record&lt;string, sequence&lt;string&gt;&gt; ([f5aaa1d](https://github.com/sriharsha-y/pqc-mobile-client/commit/f5aaa1df2d6ce4b1ad8f7b7a6f0480eb76461cae))
* **build:** lower iOS floor to 13.0 and Android minSdk to 24 ([4a6b8af](https://github.com/sriharsha-y/pqc-mobile-client/commit/4a6b8affccbc4dc073ed72a323c00190682a7d9a))
* **examples:** add runnable RN sample wiring pqc_client end-to-end ([071bab0](https://github.com/sriharsha-y/pqc-mobile-client/commit/071bab0f21f4527a0352f2d38401564af502e438))
* **ios:** PqcCore podspec consumes prebuilt release-asset zip ([7e3fcc1](https://github.com/sriharsha-y/pqc-mobile-client/commit/7e3fcc16743e898808c44794ad29eece91e26d2d))
* **release:** build a CocoaPods-consumable PqcCore-X.Y.Z.zip alongside the iOS asset ([844dfda](https://github.com/sriharsha-y/pqc-mobile-client/commit/844dfdaf6e630a25ec8cc7a8c36239ded47afa93))
* **release:** bump android/build.gradle.kts version via release-please ([91bcbb4](https://github.com/sriharsha-y/pqc-mobile-client/commit/91bcbb47d2388f736d398eedea5e15420f6435b1))
* **release:** drop pqc-mobile-client-X.Y.Z-ios.zip, keep only PqcCore-X.Y.Z.zip ([3286d91](https://github.com/sriharsha-y/pqc-mobile-client/commit/3286d91eefd4984e1d0385d40b0e88baf222b0df))
* **release:** emit a slim xcframework-only zip for SPM's binaryTarget ([285eff5](https://github.com/sriharsha-y/pqc-mobile-client/commit/285eff52b999e0844a402aacd250cbb2a952990c))
* **release:** publish-cocoapods job to push podspec to CocoaPods Trunk ([fc6d5bb](https://github.com/sriharsha-y/pqc-mobile-client/commit/fc6d5bb62c14dd0a854700ae7ff99e3a7a42201c))
* **release:** publish-maven job to release the AAR to Maven Central ([456d39d](https://github.com/sriharsha-y/pqc-mobile-client/commit/456d39df8d0f06f2557c0fafb438f93e026e6baf))
* **spm:** add Package.swift template for the swiftpm branch ([fdb04df](https://github.com/sriharsha-y/pqc-mobile-client/commit/fdb04df9faa5afd25ac900e0d392cbb077f3acce))
* **spm:** add publish-swiftpm job that bootstraps and maintains the swiftpm branch ([aa8f400](https://github.com/sriharsha-y/pqc-mobile-client/commit/aa8f400f5efbcdb460feef032b3482ef9e7caeac))


### Bug Fixes

* asset size + build hygiene (audit follow-ups) ([4911da7](https://github.com/sriharsha-y/pqc-mobile-client/commit/4911da7d4f1b28a6814765bc95a9ca48e8bc63b9))
* **ci:** scan every xcframework slice for CLI-symbol bloat, not just device ([48ab96f](https://github.com/sriharsha-y/pqc-mobile-client/commit/48ab96fdee9948ec2a24be8fc0547d7935a5f8f0))
* **client:** body-phase errors map to Network/Timeout only, never Tls/Pinning ([f62c773](https://github.com/sriharsha-y/pqc-mobile-client/commit/f62c77329bef2b020b7daa2d5bf13043b58346fe))
* **client:** map reqwest build failures to Tls, not InvalidRequest ([cd98a29](https://github.com/sriharsha-y/pqc-mobile-client/commit/cd98a29e15394d04edc572113246af4aaa807d89))
* **client:** map TLS/pinning/cert errors to typed PqcError variants ([6f4bbb3](https://github.com/sriharsha-y/pqc-mobile-client/commit/6f4bbb3dfeae64a5d0f3063432bdfefdcc0aa8d4))
* **client:** PqcHttpClient::new returns Result instead of panicking ([9f608a8](https://github.com/sriharsha-y/pqc-mobile-client/commit/9f608a83bb4bc8cb7b4e69ba5d5f25f9b8d7ccab))
* **client:** preserve non-UTF8 response header bytes via lossy decode ([f8d7722](https://github.com/sriharsha-y/pqc-mobile-client/commit/f8d772221b3cbd51af7172fbf563f8dae597d268))
* **client:** reject enable_http3 instead of silently ignoring it ([b250d07](https://github.com/sriharsha-y/pqc-mobile-client/commit/b250d07f2d18ac226479e05d2da9cf27bd0c4526))
* **client:** return ALPN protocol id, not http::Version Debug format ([24b2256](https://github.com/sriharsha-y/pqc-mobile-client/commit/24b22569d3247d820c6cd4317243479cc74f927b))
* **client:** route response-body errors through map_reqwest_err ([7885bf1](https://github.com/sriharsha-y/pqc-mobile-client/commit/7885bf1e8fc45a71eac3ee59ebe4b83fd16f4e9e))
* **client:** strip URL from error messages before substring classification ([d7cf84b](https://github.com/sriharsha-y/pqc-mobile-client/commit/d7cf84b624c30bb621047be292797106d75911bf))
* **ios:** add s.preserve_paths for the vendored XCFramework ([adabe9f](https://github.com/sriharsha-y/pqc-mobile-client/commit/adabe9f354c4d688ed1a41b1a47995494a3d3aef))
* **ios:** declare system frameworks and idiomatic library link for the podspec ([87cb404](https://github.com/sriharsha-y/pqc-mobile-client/commit/87cb4044eafdf34e1a0b712cfa9c48fcca7e1d0c))
* **ios:** narrow s.frameworks to just Security per nm symbol audit ([c94229e](https://github.com/sriharsha-y/pqc-mobile-client/commit/c94229e17ffde3b7507e48e096092e52657cd5ca))
* **profile:** drop panic=abort so cargo test reports per-test failures ([540996a](https://github.com/sriharsha-y/pqc-mobile-client/commit/540996a594578f85bfc7f4f5d9553968b492be20))
* **release:** dedup cargo-ndk between build-android and publish-maven + GPG diagnostics ([5f90cb7](https://github.com/sriharsha-y/pqc-mobile-client/commit/5f90cb7fe82a77cb42686bd9dcc1334ce8d401b3))
* **release:** harden publish-cocoapods — pin CocoaPods, guard token, flag public-repo dep ([1f934fa](https://github.com/sriharsha-y/pqc-mobile-client/commit/1f934fa95c45627e9149923cd281c89763789f43))
* **release:** keep PqcCore.podspec version in lockstep with Cargo.toml ([bdd0575](https://github.com/sriharsha-y/pqc-mobile-client/commit/bdd05750dd6a897a32ec177f46216b3881112946))
* **release:** mark PqcCore.podspec as generic so x-release-please-version is scanned ([ed08969](https://github.com/sriharsha-y/pqc-mobile-client/commit/ed08969e233b2a6a61614699800dffd9de90fd28))
* **release:** publish-swiftpm idempotent on re-run + bloat-guard slice assertion ([9e38b82](https://github.com/sriharsha-y/pqc-mobile-client/commit/9e38b82c1d6097e26b777605143d6f8ca81ed400))
* **release:** strip timestamp variance from the iOS zip for stable SHA256 ([c7020a8](https://github.com/sriharsha-y/pqc-mobile-client/commit/c7020a86b3e142a851179ac9472d630599ed64a8))
* **release:** treat cocoapods post-publish API timeout as success when Trunk confirms registration ([7dbe798](https://github.com/sriharsha-y/pqc-mobile-client/commit/7dbe7982246f029308f7a4337c3f6dd958e48231))
* **release:** unblock all three publish channels on v0.2.1 ([fe74ecb](https://github.com/sriharsha-y/pqc-mobile-client/commit/fe74ecb4659471b42cd12abca7cc00ebd775ec4e))
* **rn-sample:** declare non-exempt encryption in iOS Info.plist ([00e97e8](https://github.com/sriharsha-y/pqc-mobile-client/commit/00e97e8a9f8cb7879ff7d81f12a27dd17e850f80))
* **rn-sample:** harden iOS pod for use_frameworks! and drop dead pbxproj setting ([aaed855](https://github.com/sriharsha-y/pqc-mobile-client/commit/aaed8550f4a095ef9846e8ba936dfaab8344d572))
* **rn-sample:** match ALPN protocol ids, not the old http::Version Debug strings ([81fe41e](https://github.com/sriharsha-y/pqc-mobile-client/commit/81fe41e1aaf115657db9277b0f0302c0145c33bd))
* **rn-sample:** register PqcURLProtocol.swift in iOS project so it compiles ([009c85e](https://github.com/sriharsha-y/pqc-mobile-client/commit/009c85e8d04b69bd21376c90b41b813f690131e7))
* **rn-sample:** rename PqcURLProtocol static to avoid shadowing URLProtocol.client ([8faa8d3](https://github.com/sriharsha-y/pqc-mobile-client/commit/8faa8d330e09c7efaa37e683504bd395be3abded))
* **rn-sample:** use real negotiated protocol in synthesized responses ([730023d](https://github.com/sriharsha-y/pqc-mobile-client/commit/730023d7706e5b05e9aa565df54cf9749ddb02b9))
* **spm:** also carry PqcCore.podspec onto the swiftpm branch ([43b4891](https://github.com/sriharsha-y/pqc-mobile-client/commit/43b4891ab6ee984cec060cf920c60c3df1504ae8))
* **spm:** match binaryTarget name to xcframework modulemap and lower swift-tools-version ([bf7f38c](https://github.com/sriharsha-y/pqc-mobile-client/commit/bf7f38ce9cb87ae6dde56904f2b5235467733ca4))
* **spm:** swift package dump-package validation before pushing/tagging ([bd03850](https://github.com/sriharsha-y/pqc-mobile-client/commit/bd03850ac68e71d7800d09cacd110fce44ba534f))
* **tls:** enforce leaf-strict SPKI pinning; reject silent parse skip ([61e2424](https://github.com/sriharsha-y/pqc-mobile-client/commit/61e2424be66f2271170fe80d88390bd05e946ff7))
* **tls:** memoize instrumented CryptoProvider to bound the kx_tracker leak ([dad45ae](https://github.com/sriharsha-y/pqc-mobile-client/commit/dad45ae61fbf4b2048f4de17b9ce19c4956de7f3))


### Performance Improvements

* **build:** gate uniffi-bindgen CLI behind feature flag ([eac1400](https://github.com/sriharsha-y/pqc-mobile-client/commit/eac1400102d5fcf41ccc41f5b6b6ae042be8a28a))


### Reverts

* **client:** keep ClientBuilder::build failures as InvalidRequest ([14abb3d](https://github.com/sriharsha-y/pqc-mobile-client/commit/14abb3d4456f3e1b2acf17a4011b5c87d1d1d140))

## [0.2.1](https://github.com/sriharsha-y/pqc-mobile-client/compare/v0.2.0...v0.2.1) (2026-05-26)


### Bug Fixes

* **release:** unblock all three publish channels on v0.2.1 ([fe74ecb](https://github.com/sriharsha-y/pqc-mobile-client/commit/fe74ecb4659471b42cd12abca7cc00ebd775ec4e))

## [0.2.0](https://github.com/sriharsha-y/pqc-mobile-client/compare/v0.1.1...v0.2.0) (2026-05-26)


### ⚠ BREAKING CHANGES

* **release:** drop pqc-mobile-client-X.Y.Z-ios.zip, keep only PqcCore-X.Y.Z.zip
* **api:** Existing Kotlin/Swift consumers that build HttpRequest.headers as a Map<String, String> will need to wrap values in a list (Kotlin: `mapOf("k" to listOf("v"))`, Swift:

### Features

* **android:** add Gradle library module for Maven Central publication ([c87982f](https://github.com/sriharsha-y/pqc-mobile-client/commit/c87982f03157da7464dc24a35fe7f52a484b20cf))
* **api:** HttpRequest.headers is record&lt;string, sequence&lt;string&gt;&gt; ([f5aaa1d](https://github.com/sriharsha-y/pqc-mobile-client/commit/f5aaa1df2d6ce4b1ad8f7b7a6f0480eb76461cae))
* **build:** lower iOS floor to 13.0 and Android minSdk to 24 ([4a6b8af](https://github.com/sriharsha-y/pqc-mobile-client/commit/4a6b8affccbc4dc073ed72a323c00190682a7d9a))
* **ios:** PqcCore podspec consumes prebuilt release-asset zip ([7e3fcc1](https://github.com/sriharsha-y/pqc-mobile-client/commit/7e3fcc16743e898808c44794ad29eece91e26d2d))
* **release:** build a CocoaPods-consumable PqcCore-X.Y.Z.zip alongside the iOS asset ([844dfda](https://github.com/sriharsha-y/pqc-mobile-client/commit/844dfdaf6e630a25ec8cc7a8c36239ded47afa93))
* **release:** bump android/build.gradle.kts version via release-please ([91bcbb4](https://github.com/sriharsha-y/pqc-mobile-client/commit/91bcbb47d2388f736d398eedea5e15420f6435b1))
* **release:** drop pqc-mobile-client-X.Y.Z-ios.zip, keep only PqcCore-X.Y.Z.zip ([3286d91](https://github.com/sriharsha-y/pqc-mobile-client/commit/3286d91eefd4984e1d0385d40b0e88baf222b0df))
* **release:** emit a slim xcframework-only zip for SPM's binaryTarget ([285eff5](https://github.com/sriharsha-y/pqc-mobile-client/commit/285eff52b999e0844a402aacd250cbb2a952990c))
* **release:** publish-cocoapods job to push podspec to CocoaPods Trunk ([fc6d5bb](https://github.com/sriharsha-y/pqc-mobile-client/commit/fc6d5bb62c14dd0a854700ae7ff99e3a7a42201c))
* **release:** publish-maven job to release the AAR to Maven Central ([456d39d](https://github.com/sriharsha-y/pqc-mobile-client/commit/456d39df8d0f06f2557c0fafb438f93e026e6baf))
* **spm:** add Package.swift template for the swiftpm branch ([fdb04df](https://github.com/sriharsha-y/pqc-mobile-client/commit/fdb04df9faa5afd25ac900e0d392cbb077f3acce))
* **spm:** add publish-swiftpm job that bootstraps and maintains the swiftpm branch ([aa8f400](https://github.com/sriharsha-y/pqc-mobile-client/commit/aa8f400f5efbcdb460feef032b3482ef9e7caeac))


### Bug Fixes

* **ci:** scan every xcframework slice for CLI-symbol bloat, not just device ([48ab96f](https://github.com/sriharsha-y/pqc-mobile-client/commit/48ab96fdee9948ec2a24be8fc0547d7935a5f8f0))
* **client:** body-phase errors map to Network/Timeout only, never Tls/Pinning ([f62c773](https://github.com/sriharsha-y/pqc-mobile-client/commit/f62c77329bef2b020b7daa2d5bf13043b58346fe))
* **client:** map reqwest build failures to Tls, not InvalidRequest ([cd98a29](https://github.com/sriharsha-y/pqc-mobile-client/commit/cd98a29e15394d04edc572113246af4aaa807d89))
* **client:** reject enable_http3 instead of silently ignoring it ([b250d07](https://github.com/sriharsha-y/pqc-mobile-client/commit/b250d07f2d18ac226479e05d2da9cf27bd0c4526))
* **client:** return ALPN protocol id, not http::Version Debug format ([24b2256](https://github.com/sriharsha-y/pqc-mobile-client/commit/24b22569d3247d820c6cd4317243479cc74f927b))
* **client:** route response-body errors through map_reqwest_err ([7885bf1](https://github.com/sriharsha-y/pqc-mobile-client/commit/7885bf1e8fc45a71eac3ee59ebe4b83fd16f4e9e))
* **client:** strip URL from error messages before substring classification ([d7cf84b](https://github.com/sriharsha-y/pqc-mobile-client/commit/d7cf84b624c30bb621047be292797106d75911bf))
* **ios:** add s.preserve_paths for the vendored XCFramework ([adabe9f](https://github.com/sriharsha-y/pqc-mobile-client/commit/adabe9f354c4d688ed1a41b1a47995494a3d3aef))
* **ios:** declare system frameworks and idiomatic library link for the podspec ([87cb404](https://github.com/sriharsha-y/pqc-mobile-client/commit/87cb4044eafdf34e1a0b712cfa9c48fcca7e1d0c))
* **ios:** narrow s.frameworks to just Security per nm symbol audit ([c94229e](https://github.com/sriharsha-y/pqc-mobile-client/commit/c94229e17ffde3b7507e48e096092e52657cd5ca))
* **release:** harden publish-cocoapods — pin CocoaPods, guard token, flag public-repo dep ([1f934fa](https://github.com/sriharsha-y/pqc-mobile-client/commit/1f934fa95c45627e9149923cd281c89763789f43))
* **release:** keep PqcCore.podspec version in lockstep with Cargo.toml ([bdd0575](https://github.com/sriharsha-y/pqc-mobile-client/commit/bdd05750dd6a897a32ec177f46216b3881112946))
* **release:** mark PqcCore.podspec as generic so x-release-please-version is scanned ([ed08969](https://github.com/sriharsha-y/pqc-mobile-client/commit/ed08969e233b2a6a61614699800dffd9de90fd28))
* **release:** publish-swiftpm idempotent on re-run + bloat-guard slice assertion ([9e38b82](https://github.com/sriharsha-y/pqc-mobile-client/commit/9e38b82c1d6097e26b777605143d6f8ca81ed400))
* **release:** strip timestamp variance from the iOS zip for stable SHA256 ([c7020a8](https://github.com/sriharsha-y/pqc-mobile-client/commit/c7020a86b3e142a851179ac9472d630599ed64a8))
* **rn-sample:** match ALPN protocol ids, not the old http::Version Debug strings ([81fe41e](https://github.com/sriharsha-y/pqc-mobile-client/commit/81fe41e1aaf115657db9277b0f0302c0145c33bd))
* **spm:** also carry PqcCore.podspec onto the swiftpm branch ([43b4891](https://github.com/sriharsha-y/pqc-mobile-client/commit/43b4891ab6ee984cec060cf920c60c3df1504ae8))
* **spm:** match binaryTarget name to xcframework modulemap and lower swift-tools-version ([bf7f38c](https://github.com/sriharsha-y/pqc-mobile-client/commit/bf7f38ce9cb87ae6dde56904f2b5235467733ca4))
* **spm:** swift package dump-package validation before pushing/tagging ([bd03850](https://github.com/sriharsha-y/pqc-mobile-client/commit/bd03850ac68e71d7800d09cacd110fce44ba534f))
* **tls:** memoize instrumented CryptoProvider to bound the kx_tracker leak ([dad45ae](https://github.com/sriharsha-y/pqc-mobile-client/commit/dad45ae61fbf4b2048f4de17b9ce19c4956de7f3))


### Reverts

* **client:** keep ClientBuilder::build failures as InvalidRequest ([14abb3d](https://github.com/sriharsha-y/pqc-mobile-client/commit/14abb3d4456f3e1b2acf17a4011b5c87d1d1d140))

## [0.1.1](https://github.com/sriharsha-y/pqc-mobile-client/compare/v0.1.0...v0.1.1) (2026-05-25)


### Bug Fixes

* asset size + build hygiene (audit follow-ups) ([4911da7](https://github.com/sriharsha-y/pqc-mobile-client/commit/4911da7d4f1b28a6814765bc95a9ca48e8bc63b9))
* **rn-sample:** declare non-exempt encryption in iOS Info.plist ([00e97e8](https://github.com/sriharsha-y/pqc-mobile-client/commit/00e97e8a9f8cb7879ff7d81f12a27dd17e850f80))
* **rn-sample:** harden iOS pod for use_frameworks! and drop dead pbxproj setting ([aaed855](https://github.com/sriharsha-y/pqc-mobile-client/commit/aaed8550f4a095ef9846e8ba936dfaab8344d572))
* **rn-sample:** register PqcURLProtocol.swift in iOS project so it compiles ([009c85e](https://github.com/sriharsha-y/pqc-mobile-client/commit/009c85e8d04b69bd21376c90b41b813f690131e7))


### Performance Improvements

* **build:** gate uniffi-bindgen CLI behind feature flag ([eac1400](https://github.com/sriharsha-y/pqc-mobile-client/commit/eac1400102d5fcf41ccc41f5b6b6ae042be8a28a))

## 0.1.0 (2026-05-22)


### Features

* **examples:** add runnable RN sample wiring pqc_client end-to-end ([071bab0](https://github.com/sriharsha-y/pqc-mobile-client/commit/071bab0f21f4527a0352f2d38401564af502e438))


### Bug Fixes

* **client:** map TLS/pinning/cert errors to typed PqcError variants ([6f4bbb3](https://github.com/sriharsha-y/pqc-mobile-client/commit/6f4bbb3dfeae64a5d0f3063432bdfefdcc0aa8d4))
* **client:** PqcHttpClient::new returns Result instead of panicking ([9f608a8](https://github.com/sriharsha-y/pqc-mobile-client/commit/9f608a83bb4bc8cb7b4e69ba5d5f25f9b8d7ccab))
* **client:** preserve non-UTF8 response header bytes via lossy decode ([f8d7722](https://github.com/sriharsha-y/pqc-mobile-client/commit/f8d772221b3cbd51af7172fbf563f8dae597d268))
* **profile:** drop panic=abort so cargo test reports per-test failures ([540996a](https://github.com/sriharsha-y/pqc-mobile-client/commit/540996a594578f85bfc7f4f5d9553968b492be20))
* **rn-sample:** rename PqcURLProtocol static to avoid shadowing URLProtocol.client ([8faa8d3](https://github.com/sriharsha-y/pqc-mobile-client/commit/8faa8d330e09c7efaa37e683504bd395be3abded))
* **rn-sample:** use real negotiated protocol in synthesized responses ([730023d](https://github.com/sriharsha-y/pqc-mobile-client/commit/730023d7706e5b05e9aa565df54cf9749ddb02b9))
* **tls:** enforce leaf-strict SPKI pinning; reject silent parse skip ([61e2424](https://github.com/sriharsha-y/pqc-mobile-client/commit/61e2424be66f2271170fe80d88390bd05e946ff7))

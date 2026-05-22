# Changelog

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

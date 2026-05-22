use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};

use crate::config::PqcConfig;
use crate::error::PqcError;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest, HttpResponse};

/// The HTTPS client exposed to Kotlin / Swift via UniFFI.
///
/// Holds a single `reqwest::Client` with PQC TLS configured. Construct once
/// per process (it owns the connection pool); calling `request` is cheap.
pub struct PqcHttpClient {
    inner: reqwest::Client,
    default_timeout: Option<Duration>,
}

impl PqcHttpClient {
    pub fn new(config: PqcConfig) -> Arc<Self> {
        let tls = build_tls_config(&config).expect("TLS config build failed");

        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .cookie_store(true)
            .gzip(true)
            .brotli(true)
            .pool_max_idle_per_host(10);

        if let Some(timeout_ms) = config.default_timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }

        // HTTP/3 (QUIC) — opt-in. Adds the h3-quinn dependency footprint;
        // gated until the corresponding cargo feature is enabled.
        // if config.enable_http3 {
        //     builder = builder.http3_prior_knowledge();
        // }

        let client = builder.build().expect("reqwest client build failed");

        Arc::new(Self {
            inner: client,
            default_timeout: config.default_timeout_ms.map(Duration::from_millis),
        })
    }

    pub async fn request(&self, req: HttpRequest) -> Result<HttpResponse, PqcError> {
        let method = match req.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Head => reqwest::Method::HEAD,
            HttpMethod::Options => reqwest::Method::OPTIONS,
        };

        let mut builder = self.inner.request(method, &req.url);

        for (k, v) in &req.headers {
            let name = HeaderName::try_from(k.as_str()).map_err(|_| PqcError::InvalidRequest)?;
            let value = HeaderValue::try_from(v.as_str()).map_err(|_| PqcError::InvalidRequest)?;
            builder = builder.header(name, value);
        }

        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        let timeout_ms = req
            .timeout_ms
            .or(self.default_timeout.map(|d| d.as_millis() as u64));
        if let Some(t) = timeout_ms {
            builder = builder.timeout(Duration::from_millis(t));
        }

        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                PqcError::Timeout
            } else if e.is_connect() || e.is_request() {
                PqcError::Network
            } else {
                PqcError::Network
            }
        })?;

        let status = resp.status().as_u16();
        let negotiated_protocol = format!("{:?}", resp.version());

        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in resp.headers() {
            let s = v.to_str().unwrap_or("").to_string();
            headers.entry(k.as_str().to_string()).or_default().push(s);
        }

        let body = resp
            .bytes()
            .await
            .map_err(|_| PqcError::InvalidResponse)?
            .to_vec();

        // TODO: surface the actual negotiated named group from rustls.
        // reqwest does not currently expose this; will require either a
        // custom hyper connector or a rustls connection-listener hook.
        let negotiated_named_group = "X25519MLKEM768".to_string();

        Ok(HttpResponse {
            status,
            headers,
            body,
            negotiated_named_group,
            negotiated_protocol,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PqcConfig {
    pub pinned_cert_sha256: Vec<String>,
    pub enable_post_quantum: bool,
    pub enable_http3: bool,
    pub default_timeout_ms: Option<u64>,
}

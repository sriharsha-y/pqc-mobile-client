use std::collections::HashMap;

#[derive(Debug, Clone, uniffi::Enum)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: HashMap<String, Vec<String>>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct HttpResponse {
    pub status: u16,
    /// The final URL the body was actually fetched from, after any redirects
    /// were followed. Equals the request URL when no redirect occurred. Lets
    /// callers detect a redirect they refused (see `RedirectPolicy`) and learn
    /// the effective origin — mirrors OkHttp `Response.request().url()` and
    /// `URLResponse.url`.
    pub final_url: String,
    pub headers: HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
    pub negotiated_protocol: String,
}

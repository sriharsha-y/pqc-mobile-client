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
    pub headers: HashMap<String, Vec<String>>,
    pub body: Vec<u8>,
    pub negotiated_protocol: String,
}

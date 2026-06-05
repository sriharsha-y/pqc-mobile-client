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

// HttpResponse (a UniFFI Record) was removed in this release. Responses
// are now streamed via PqcResponse, a UniFFI Object with async methods
// (read_chunk / bytes / cancel) — see src/client.rs. This matches the
// native default: URLSession's `dataTask` and OkHttp's `ResponseBody`
// are both streaming-first; their byte-buffered variants (`data(for:)`
// / `body().bytes()`) are convenience layers on top.
//
// Migration: `resp.body` → `resp.bytes().await` (same behavior, async)
// or `while let Some(c) = resp.read_chunk().await? { ... }` for true
// streaming over large downloads.

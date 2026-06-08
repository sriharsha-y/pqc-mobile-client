use std::collections::HashMap;
use std::sync::Arc;

use crate::error::PqcError;

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

/// Foreign-implementable streaming upload body. The Rust client pulls
/// chunks via `next_chunk` until it returns `None` (EOF) or `Err`
/// (abort). Implemented by Kotlin and Swift to bridge OkHttp's
/// `RequestBody.writeTo(BufferedSink)` and `URLRequest.httpBodyStream`
/// (an `InputStream`) into Rust's `reqwest::Body::wrap_stream` without
/// buffering the full upload in memory — matching native OkHttp /
/// URLSession streaming-upload behavior.
///
/// # Threading
///
/// `next_chunk` is synchronous on the FFI surface. The Rust client
/// invokes it via `tokio::task::spawn_blocking` so the foreign call
/// doesn't park a tokio worker — implementers can block on local I/O
/// (file reads, okio.Pipe.source.read, InputStream.read) freely.
///
/// # Chunk size
///
/// Typical chunk size is 16-64 KiB. Smaller is fine (more FFI
/// round-trips); larger is fine (peak memory tracks the largest single
/// chunk). Returning an empty Vec is treated as EOF.
///
/// # Retry safety
///
/// Streaming bodies are NOT automatically retry-safe — once a chunk is
/// consumed it's gone (matches URLSession's `needNewBodyStream:`
/// contract). If a request needs retry, the consumer must construct a
/// fresh `BodyProvider`. Don't enable redirects on streaming uploads
/// unless the source can be re-read.
#[uniffi::export(with_foreign)]
pub trait BodyProvider: Send + Sync {
    /// Return the next chunk of upload body, or `None` at EOF. Empty
    /// vecs are also treated as EOF (lets callers signal end-of-stream
    /// without keeping an Option flag).
    fn next_chunk(&self) -> Result<Option<Vec<u8>>, PqcError>;
}

#[derive(Clone, uniffi::Record)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: HashMap<String, Vec<String>>,
    /// Inline body bytes for small uploads (request payload fully in
    /// memory). Mutually exclusive with `body_stream`; passing both is
    /// rejected with `PqcError::InvalidRequest`.
    pub body: Option<Vec<u8>>,
    /// Streaming upload body. When set, the client pulls chunks from
    /// the provider and forwards them to the server without buffering
    /// the full payload — required for large file uploads on
    /// memory-constrained devices. Mutually exclusive with `body`.
    #[uniffi(default = None)]
    pub body_stream: Option<Arc<dyn BodyProvider>>,
    /// Optional `Content-Length` hint when using `body_stream`. When
    /// `None`, the request uses chunked transfer-encoding (the natural
    /// fit for stream sources of unknown length); when `Some(n)`, the
    /// `Content-Length: n` header is set and the server gets a
    /// content-length-framed body. Set this when the source's total
    /// size is known (file uploads); leave `None` for live streams.
    #[uniffi(default = None)]
    pub body_stream_length: Option<u64>,
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

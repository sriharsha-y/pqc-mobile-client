//! Streaming RFC 9111 response cache (the `cache` cargo feature).
//!
//! Implements `http_cache::StreamingCacheManager` against our existing
//! storage primitives: `cacache` for the on-disk content-addressable
//! store, `moka` for an in-memory hot tier. The RFC semantics
//! (freshness, revalidation, `Vary`, `Authorization` rules) come from
//! the `http-cache` / `http-cache-semantics` stack — cacheability is
//! decided by method + status + headers, never by file type.
//!
//! # Storage layout
//!
//! One cacache entry per cached response, keyed by the http-cache
//! cache_key string:
//!   - Body bytes live in the content-addressable blob.
//!   - Postcard-encoded `CacheMetadata` (status, headers, RFC policy,
//!     optional user-metadata) lives in the entry's `raw_metadata`,
//!     stored inline in the cacache index. cacache's own `Metadata.size`
//!     is the body size; we don't carry our own.
//!
//! On `get`, `cacache::metadata(path, key)` returns the entry head
//! (small; lives in the index). Its `raw_metadata` is our postcard
//! `CacheMetadata`. The body is then streamed via `cacache::Reader`
//! (`AsyncRead`) in 64 KB chunks — large responses never materialize
//! in our process memory.
//!
//! On `put`, the body is buffered into `Bytes` and written via
//! `WriteOpts::new().raw_metadata(postcard_bytes).open(path, key)` →
//! `write_all` → `commit`. Atomic — `commit` only finalizes after the
//! body has been SHA-verified.
//!
//! On `delete`, both the key (index entry + raw_metadata) and the
//! body blob (by integrity) are removed.
//!
//! # In-memory hot tier
//!
//! The optional `moka` tier (controlled by `PqcConfig.max_memory_cache_bytes`)
//! caches the full body bytes of small responses (those under the
//! per-entry memory cap, internally `mem_total / 20`, matching the
//! observed iOS `URLCache` "~5% of capacity" rule). Responses above
//! the cap skip the memory tier and live on disk only. Reads check
//! moka first, then cacache.
//!
//! # Per-entry size caps
//!
//! Both caps are internal, mirroring iOS URLCache's empirical (and
//! undocumented) "~5% of capacity" rule. Not exposed via `PqcConfig`
//! because URLCache itself doesn't expose them. The disk cap also
//! gates cacheability: oversized responses are silently uncached.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use http::{HeaderName, HeaderValue, Response, StatusCode, Version};
use http_body::{Body as HttpBody, Frame};
use http_cache::{
    HttpCacheError, Result as HttpCacheResult, StreamingCacheManager, StreamingError, Url,
};
use http_cache_semantics::CachePolicy;
use pin_project_lite::pin_project;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;

use crate::config::PqcConfig;

/// Per-entry size divisor — internal "5% of total" rule mirroring
/// URLCache's undocumented per-entry threshold.
const ENTRY_CAP_DIVISOR: u64 = 20;

/// Diagnostic response header set by the cache manager: `true` on a
/// cache hit (mem or disk tier), `false` on a miss. Absent when the
/// cache layer isn't engaged (no `cache` feature, or `enable_cache =
/// false`).
const X_PQC_CACHE_HIT: HeaderName = HeaderName::from_static("x-pqc-cache-hit");

/// On-disk cap when `max_cache_bytes` is `None`: 20 MiB, a typical
/// `URLCache` disk capacity.
const DEFAULT_MAX_CACHE_BYTES: u64 = 20 * 1024 * 1024;

/// In-memory hot tier cap when `max_memory_cache_bytes` is `None`:
/// 4 MiB on both platforms. `Some(0)` opts out entirely (Android
/// consumers preserving OkHttp-style disk-only behavior).
const DEFAULT_MEM_CACHE_BYTES: u64 = 4 * 1024 * 1024;

/// Chunk size for streaming cacache reads (matches upstream's
/// `StreamingManager` default). 64 KB balances syscall overhead
/// against memory footprint.
const STREAM_CHUNK_SIZE: usize = 64 * 1024;

/// Content-Length threshold (inclusive) below which `put` skips the
/// tee machinery and just buffers the body before writing.
///
/// Why 64 KiB: matches okio's 8 * Segment.SIZE (OkHttp's "small body"
/// unit) and our existing INLINE_BODY_THRESHOLD on the upload path
/// (PqcInterceptor.kt). Below this, a single allocation + sequential
/// write outperforms the spawn-task + channel setup; above, the tee
/// pattern pays for itself by capping peak memory at one frame
/// instead of the full body.
const INLINE_BUFFERED_THRESHOLD: u64 = 64 * 1024;

/// Bounded channel depth between the tee background task and the
/// consumer-side PqcCachedBody. 16 frames × ~64 KiB peak resident ≈
/// 1 MiB of buffer per in-flight chunked cache miss — provides natural
/// network-to-disk backpressure without OOM risk on mobile.
const TEE_CHANNEL_DEPTH: usize = 16;

/// Internal per-entry cap on the on-disk tier.
fn per_entry_disk_cap(disk_total: u64) -> u64 {
    disk_total / ENTRY_CAP_DIVISOR
}

/// Internal per-entry cap on the memory tier. Doubles as the streaming
/// gate: responses above this size skip the moka tier and live on disk
/// only, so a 50 MiB download never pins moka.
fn per_entry_mem_cap(mem_total: u64) -> u64 {
    mem_total / ENTRY_CAP_DIVISOR
}

/// Postcard-serialized cache record, stored in the cacache entry's
/// `raw_metadata`. On-disk format is private to this module.
///
/// `body_size` is the authoritative body length for read paths.
/// `None` is a sentinel meaning "tee write in progress" — written by
/// `put_tee`'s initial commit (which doesn't yet know the body size),
/// overwritten with `Some(total)` by the post-commit index reinsert
/// once the upstream EOFs. `get` treats `None` as a cache miss so a
/// concurrent fetch during the (microsecond) commit→reinsert window
/// falls through to the network instead of returning an empty body.
/// Also acts as a permanent-broken marker if the reinsert fails:
/// future gets keep missing and fetch fresh.
///
/// `put_buffered` always sets `Some(len)` since the size is known
/// up front (no race window).
#[derive(Serialize, Deserialize)]
struct CacheMetadata {
    status: u16,
    version: u8,
    /// Headers as a flat list to preserve multi-valued entries (e.g.
    /// `Set-Cookie`, `Vary`) without an outer HashMap collapsing them.
    headers: Vec<(String, Vec<u8>)>,
    policy: CachePolicy,
    #[serde(default)]
    user_metadata: Option<Vec<u8>>,
    /// Body size in bytes when known, `None` during a tee in-flight
    /// commit. See struct doc above.
    #[serde(default)]
    body_size: Option<u64>,
}

fn version_to_u8(v: Version) -> u8 {
    match v {
        Version::HTTP_09 => 9,
        Version::HTTP_10 => 10,
        Version::HTTP_11 => 11,
        Version::HTTP_2 => 2,
        Version::HTTP_3 => 3,
        _ => 11,
    }
}

fn version_from_u8(b: u8) -> Version {
    match b {
        9 => Version::HTTP_09,
        10 => Version::HTTP_10,
        11 => Version::HTTP_11,
        2 => Version::HTTP_2,
        3 => Version::HTTP_3,
        _ => Version::HTTP_11,
    }
}

/// Persistent byte-bounded disk tier. `bytes` is a running logical-size
/// counter so put/size stay O(1); `evict_lock` serializes both eviction
/// and `clear` so concurrent puts can't race a full rescan. `seeded`
/// is the one-shot flag for lazy on-first-use counter initialization
/// (constructor can't spawn — see `ensure_seeded`).
#[derive(Clone)]
struct DiskTier {
    path: PathBuf,
    max_bytes: u64,
    bytes: Arc<AtomicU64>,
    evict_lock: Arc<tokio::sync::Mutex<()>>,
    seeded: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub struct PqcStreamingCacheManager {
    disk: Option<DiskTier>,
    /// In-memory LRU hot tier. Built when `max_memory_cache_bytes` is
    /// non-zero (default 4 MiB on both platforms); `None` when the
    /// consumer opts out via `Some(0)`.
    mem: Option<moka::future::Cache<String, Arc<Bytes>>>,
    /// Per-entry caps. Disk cap doubles as the cacheability gate;
    /// memory cap doubles as the streaming gate (above it → disk only).
    per_entry_disk: u64,
    per_entry_mem: u64,
}

impl PqcStreamingCacheManager {
    pub fn new(config: &PqcConfig) -> Option<Self> {
        let disk = config.cache_dir.as_ref().map(|d| {
            // No `tokio::spawn` here: PqcHttpClient::new is a sync UniFFI
            // constructor called from a foreign FFI thread (Swift Main on
            // iOS, JNI on Android) with no tokio runtime entered, so spawn
            // would panic with "there is no reactor running". The byte
            // counter is seeded lazily by `ensure_seeded` on the first
            // async cache call instead — by then UniFFI's tokio runtime
            // is live.
            DiskTier {
                path: PathBuf::from(d),
                max_bytes: config.max_cache_bytes.unwrap_or(DEFAULT_MAX_CACHE_BYTES),
                bytes: Arc::new(AtomicU64::new(0)),
                evict_lock: Arc::new(tokio::sync::Mutex::new(())),
                seeded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        });

        let mem_cap = config
            .max_memory_cache_bytes
            .unwrap_or(DEFAULT_MEM_CACHE_BYTES);
        let mem = build_mem_tier(mem_cap);

        if disk.is_none() && mem.is_none() {
            return None;
        }

        let disk_total = disk.as_ref().map(|d| d.max_bytes).unwrap_or(0);
        let per_entry_disk = per_entry_disk_cap(disk_total);
        let per_entry_mem = per_entry_mem_cap(mem_cap);

        Some(Self {
            disk,
            mem,
            per_entry_disk,
            per_entry_mem,
        })
    }

    /// Clear all cached responses (best-effort; mirrors the non-throwing
    /// `URLCache.removeAllCachedResponses` / OkHttp `Cache.evictAll`).
    pub async fn clear(&self) {
        if let Some(mem) = &self.mem {
            mem.invalidate_all();
            mem.run_pending_tasks().await;
        }
        if let Some(disk) = &self.disk {
            let _guard = disk.evict_lock.lock().await;
            if let Err(e) = cacache::clear(&disk.path).await {
                log::warn!("pqc cache: clear failed: {e}");
            }
            // The evict_lock excludes other resyncs; puts don't take it
            // before fetch_add, but a put that races here has already
            // written to disk and would be wiped by cacache::clear above
            // either way. store(0) + seeded is the authoritative state.
            disk.bytes.store(0, Ordering::Release);
            disk.seeded.store(true, Ordering::Release);
        }
    }

    /// Total bytes in the on-disk cache, for "Clear cache (X MB)" UIs.
    /// Returns 0 when caching is disabled or disk tier absent.
    pub async fn size(&self) -> u64 {
        match &self.disk {
            Some(disk) => disk.bytes.load(Ordering::Acquire),
            None => 0,
        }
    }
}

// Our concrete `Body` type. Three variants:
//   - Buffered: resident Bytes carved into STREAM_CHUNK_SIZE slices.
//     Used for mem-tier hits and the small-body cache-miss path
//     (where the body was buffered to compute size for caching).
//   - Cached: disk-tier hits, streaming from cacache::Reader.
//   - Passthrough: large or non-cacheable bodies bypass cache
//     storage entirely and stream through from the upstream body.
//     Frame size tracks the upstream's natural frame size (typically
//     16-64 KB from reqwest/hyper). Lets a 100 MB download with
//     known-oversized Content-Length avoid the 100 MB buffering
//     spike that `put` would otherwise incur — peak memory stays
//     at one frame regardless of body size.
//
// pin-project'd because the Cached variant pins an AsyncRead trait
// object and Passthrough pins a dyn HttpBody trait object.
pin_project! {
    #[project = PqcCachedBodyProj]
    pub enum PqcCachedBody {
        /// Resident `Bytes` blob — memory-tier hits, mem-promoted put
        /// results, and `empty_body`. `poll_frame` carves it into
        /// STREAM_CHUNK_SIZE slices so consumers see streamed chunks
        /// even on the mem-hit path, matching the disk-tier Cached
        /// variant's chunked delivery.
        Buffered {
            data: Bytes,
        },
        /// Streamed from cacache via `AsyncRead`. Reads up to
        /// `STREAM_CHUNK_SIZE` bytes per `poll_frame`, never holding
        /// more than one chunk in memory.
        Cached {
            #[pin]
            reader: Pin<Box<dyn AsyncRead + Send>>,
            buf: BytesMut,
            done: bool,
            remaining: u64,
        },
        /// Passthrough — wraps the upstream `HttpBody` directly,
        /// delegating `poll_frame` without any buffering. Used by
        /// `put` when the response is known oversized (via
        /// Content-Length) and by `convert_body` for the
        /// non-cacheable path. Peak memory: one upstream frame.
        Passthrough {
            body: Pin<Box<dyn HttpBody<Data = Bytes, Error = StreamingError> + Send>>,
        },
    }
}

impl HttpBody for PqcCachedBody {
    type Data = Bytes;
    type Error = StreamingError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, StreamingError>>> {
        match self.project() {
            PqcCachedBodyProj::Buffered { data } => {
                if data.is_empty() {
                    return Poll::Ready(None);
                }
                let n = STREAM_CHUNK_SIZE.min(data.len());
                let chunk = data.split_to(n);
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            PqcCachedBodyProj::Passthrough { body } => {
                // `body: &mut Pin<Box<dyn HttpBody<...>>>`. Pin<Box<T>>
                // is Unpin (heap-pinned), so we can take &mut through
                // pin_project's normal projection and call .as_mut()
                // to peel into Pin<&mut dyn HttpBody<...>>.
                body.as_mut().poll_frame(cx)
            }
            PqcCachedBodyProj::Cached {
                mut reader,
                buf,
                done,
                remaining,
            } => {
                if *done {
                    return Poll::Ready(None);
                }
                // Ensure capacity for the next chunk read.
                if buf.capacity() < STREAM_CHUNK_SIZE {
                    buf.reserve(STREAM_CHUNK_SIZE - buf.capacity());
                }
                // SAFETY: BytesMut's spare capacity isn't initialized,
                // but AsyncRead::poll_read writes initialized bytes and
                // we only advance the length by the reported amount.
                let pre_len = buf.len();
                let want = STREAM_CHUNK_SIZE.min(*remaining as usize);
                buf.resize(pre_len + want, 0);
                let mut read_buf = tokio::io::ReadBuf::new(&mut buf[pre_len..]);
                match reader.as_mut().poll_read(cx, &mut read_buf) {
                    Poll::Pending => {
                        buf.truncate(pre_len);
                        Poll::Pending
                    }
                    Poll::Ready(Err(e)) => {
                        buf.truncate(pre_len);
                        *done = true;
                        Poll::Ready(Some(Err(StreamingError::new(Box::new(e)))))
                    }
                    Poll::Ready(Ok(())) => {
                        let n = read_buf.filled().len();
                        buf.truncate(pre_len + n);
                        if n == 0 {
                            *done = true;
                            return Poll::Ready(None);
                        }
                        *remaining = remaining.saturating_sub(n as u64);
                        let chunk = buf.split().freeze();
                        if *remaining == 0 {
                            *done = true;
                        }
                        Poll::Ready(Some(Ok(Frame::data(chunk))))
                    }
                }
            }
        }
    }
}

impl PqcStreamingCacheManager {
    /// Build a response head from `CacheMetadata`. Caller picks the
    /// body — either Buffered (mem hit) or Cached (disk stream). Sets
    /// `x-pqc-cache-hit: true` so consumers can tell this came from
    /// the cache.
    fn response_head(meta: &CacheMetadata) -> http::response::Builder {
        let mut b = Response::builder()
            .status(StatusCode::from_u16(meta.status).unwrap_or(StatusCode::OK))
            .version(version_from_u8(meta.version));
        for (name, value) in &meta.headers {
            // Skip any prior X_PQC_CACHE_HIT — `http::Builder::header`
            // APPENDS, so a stale value persisted into CacheMetadata
            // would coexist with the fresh one.
            if name.eq_ignore_ascii_case(X_PQC_CACHE_HIT.as_str()) {
                continue;
            }
            if let (Ok(n), Ok(v)) = (
                HeaderName::try_from(name.as_str()),
                HeaderValue::from_bytes(value),
            ) {
                b = b.header(n, v);
            }
        }
        b.header(&X_PQC_CACHE_HIT, HeaderValue::from_static("true"))
    }

    /// Pre-decide whether a response of known length would fail to fit
    /// in EVERY available tier. When true, `put` skips buffering and
    /// streams the upstream body through directly. Returns false when
    /// the length is unknown (chunked encoding) — we can't pre-decide
    /// without the body, so fall through to the existing buffered path
    /// (which the `tee-stream middleware` follow-up will fix).
    fn known_too_big_for_all_tiers(&self, content_length: Option<u64>) -> bool {
        let Some(len) = content_length else {
            return false;
        };
        let disk_rejects = self
            .disk
            .as_ref()
            .is_none_or(|d| len > self.per_entry_disk || len > d.max_bytes);
        let mem_rejects = self.mem.as_ref().is_none_or(|_| len > self.per_entry_mem);
        disk_rejects && mem_rejects
    }
}

/// Parse the response's Content-Length header into a u64, if present
/// and well-formed. Used by `put` / `convert_body` to pre-decide on
/// the streaming-vs-buffered path before consuming the body.
fn content_length_of(headers: &http::HeaderMap) -> Option<u64> {
    headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Rebuild a `Response<PqcCachedBody>` from the upstream response's
/// parts + an already-shaped `PqcCachedBody`. Single source of truth
/// for status/version/headers/extensions copying so a future Parts
/// field (trailers, h2 settings) only needs to be plumbed once. Also
/// the single place the `x-pqc-cache-hit: false` diagnostic is set on
/// all miss paths (`put_buffered`, `put_tee`, `passthrough_response`).
fn build_response_with_body(
    parts: http::response::Parts,
    body: PqcCachedBody,
) -> HttpCacheResult<Response<PqcCachedBody>> {
    let mut b = Response::builder()
        .status(parts.status)
        .version(parts.version);
    for (name, value) in &parts.headers {
        // Same de-dup as response_head: skip a prior X_PQC_CACHE_HIT
        // from the upstream parts so our miss value (`false`) is
        // single-valued. `name` is already a HeaderName so this is a
        // pointer-cheap equality, not a case-folded scan.
        if name == X_PQC_CACHE_HIT {
            continue;
        }
        b = b.header(name, value);
    }
    b = b.header(&X_PQC_CACHE_HIT, HeaderValue::from_static("false"));
    let mut resp = b
        .body(body)
        .map_err(|e| HttpCacheError::cache(format!("build response: {e}")))?;
    *resp.extensions_mut() = parts.extensions;
    Ok(resp)
}

/// Build a `Response<PqcCachedBody>` whose body is a `Passthrough`
/// wrapping the upstream body type-erased. The upstream body's
/// `Data`/`Error` are mapped to `Bytes`/`StreamingError` via
/// `BodyExt::map_frame` + `map_err`; data conversion uses
/// `Buf::copy_to_bytes` so each upstream frame allocates one Bytes —
/// memory bound is one frame (typically 16-64 KB), not the whole body.
///
/// Used by `put` when Content-Length is known-oversized for all tiers,
/// and by `convert_body` on every non-cacheable response.
fn passthrough_response<B>(
    parts: http::response::Parts,
    body: B,
) -> HttpCacheResult<Response<PqcCachedBody>>
where
    B: HttpBody + Send + 'static,
    B::Data: Send,
    B::Error: Into<StreamingError>,
{
    use bytes::Buf;
    use http_body_util::BodyExt;
    let mapped = body
        .map_err(Into::<StreamingError>::into)
        .map_frame(|frame| {
            frame.map_data(|mut d| {
                let len = d.remaining();
                d.copy_to_bytes(len)
            })
        });
    build_response_with_body(
        parts,
        PqcCachedBody::Passthrough {
            body: Box::pin(mapped),
        },
    )
}

impl StreamingCacheManager for PqcStreamingCacheManager {
    type Body = PqcCachedBody;

    async fn get(
        &self,
        cache_key: &str,
    ) -> HttpCacheResult<Option<(Response<Self::Body>, CachePolicy)>> {
        // Seed the byte counter on the first cache call — the sync
        // constructor can't (no tokio runtime on FFI threads).
        self.ensure_seeded().await;
        let disk = match &self.disk {
            Some(d) => d,
            None => return Ok(None),
        };

        // Single cacache::metadata call: returns the entry head from the
        // (sharded) index, including our postcard CacheMetadata in
        // raw_metadata and the body length in size. A miss here is the
        // most common case — be silent.
        let entry = match cacache::metadata(&disk.path, cache_key).await {
            Ok(Some(m)) => m,
            _ => return Ok(None),
        };
        let raw_meta = match entry.raw_metadata.as_deref() {
            Some(b) if !b.is_empty() => b,
            _ => return Ok(None),
        };
        let meta: CacheMetadata = match postcard::from_bytes(raw_meta) {
            Ok(m) => m,
            Err(e) => {
                // Treat deserialize failures as cache misses — upstream
                // does the same since alpha.5 (issue #141). Could be a
                // postcard format drift across versions; rebuilding is
                // safer than crashing.
                log::debug!("pqc cache: metadata deserialize failed for {cache_key}: {e}");
                return Ok(None);
            }
        };

        // Race-window / broken-entry guard: `put_tee` writes the
        // initial index entry with `body_size: None` (since it doesn't
        // know the size until upstream EOFs), then re-inserts the
        // index with `body_size: Some(total)` post-commit. Between
        // those two appends, a concurrent get would otherwise read
        // `entry.size = 0` and return a zero-byte body. Treat
        // `body_size: None` as a cache miss so the consumer falls
        // through to a network fetch — and so the same applies
        // permanently if the reinsert ever fails (rare disk EIO).
        let body_size = match meta.body_size {
            Some(n) => n,
            None => return Ok(None),
        };

        // Build the body. Memory tier first — if hit, the entire body is
        // already resident; no disk syscall needed.
        let body = if let Some(b) = self.read_mem(cache_key).await {
            PqcCachedBody::Buffered { data: b }
        } else {
            // Open a streaming reader BY INTEGRITY. We already have
            // `entry.integrity` from the metadata() call above —
            // `Reader::open(path, key)` would re-walk the index
            // bucket to look up the same integrity we already hold,
            // doubling the per-hit I/O. `open_hash(path, integrity)`
            // skips that and goes straight to the content blob.
            // `remaining` uses `body_size` from raw_metadata
            // (authoritative — see body_size guard above), not
            // `entry.size` which is 0 during the tee race window.
            match cacache::Reader::open_hash(&disk.path, entry.integrity.clone()).await {
                Ok(reader) => PqcCachedBody::Cached {
                    reader: Box::pin(reader),
                    buf: BytesMut::with_capacity(STREAM_CHUNK_SIZE),
                    done: false,
                    remaining: body_size,
                },
                Err(_) => {
                    // Index entry exists but content blob doesn't (rare;
                    // manual cacache GC or partial filesystem loss).
                    return Ok(None);
                }
            }
        };

        let resp = Self::response_head(&meta)
            .body(body)
            .map_err(|e| HttpCacheError::cache(format!("build response: {e}")))?;
        Ok(Some((resp, meta.policy)))
    }

    async fn put<B>(
        &self,
        cache_key: String,
        response: Response<B>,
        policy: CachePolicy,
        _request_url: Url,
        metadata: Option<Vec<u8>>,
    ) -> HttpCacheResult<Response<Self::Body>>
    where
        B: HttpBody + Send + 'static,
        B::Data: Send,
        B::Error: Into<StreamingError>,
    {
        // Seed the byte counter on the first cache call — the sync
        // constructor can't (no tokio runtime on FFI threads).
        self.ensure_seeded().await;
        let (parts, body) = response.into_parts();
        let content_len = content_length_of(&parts.headers);

        // Path A — known too big for ALL tiers: pass through entirely.
        // Avoids the buffering OOM spike for large known-length bodies.
        if self.known_too_big_for_all_tiers(content_len) {
            return passthrough_response(parts, body);
        }

        // Path B — small known-length OR no disk tier: buffer.
        //   - small known: tee overhead (channel + spawn) isn't worth it
        //     for a <= 64 KiB JSON; one allocation + sequential write is
        //     simpler and faster.
        //   - no disk tier: mem-only caching requires the full body in
        //     memory anyway, so streaming buys nothing.
        let should_buffer =
            self.disk.is_none() || content_len.is_some_and(|n| n <= INLINE_BUFFERED_THRESHOLD);
        if should_buffer {
            return self
                .put_buffered(cache_key, parts, body, policy, metadata)
                .await;
        }

        // Path C — large known OR unknown length (chunked encoding): tee.
        // Bytes flow through ONCE — every chunk goes simultaneously to
        // the consumer (via mpsc) and to cacache::Writer. Peak memory
        // stays at one frame, never the full body. Matches OkHttp's
        // CacheRequestBody Source/Sink tee pattern. See put_tee at the
        // bottom of this file.
        self.put_tee(cache_key, parts, body, policy, metadata).await
    }

    async fn convert_body<B>(&self, response: Response<B>) -> HttpCacheResult<Response<Self::Body>>
    where
        B: HttpBody + Send + 'static,
        B::Data: Send,
        B::Error: Into<StreamingError>,
    {
        // Non-cacheable response by definition (the middleware uses
        // this when CacheMode says skip). Pass through without
        // buffering — there's no caching benefit to materializing,
        // and a `Cache-Control: no-store` 4K-camera video upload
        // would otherwise eat hundreds of MiB.
        let (parts, body) = response.into_parts();
        passthrough_response(parts, body)
    }

    async fn delete(&self, cache_key: &str) -> HttpCacheResult<()> {
        if let Some(mem) = &self.mem {
            mem.invalidate(cache_key).await;
        }
        if let Some(disk) = &self.disk {
            let removed = remove_entry(disk, cache_key).await;
            disk.bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                    Some(v.saturating_sub(removed))
                })
                .ok();
        }
        Ok(())
    }

    fn empty_body(&self) -> Self::Body {
        PqcCachedBody::Buffered { data: Bytes::new() }
    }

    // Note: trait-side this method is `#[cfg(feature = "streaming")]` —
    // since we enable http-cache's `streaming` feature in Cargo.toml,
    // the trait requires this implementation unconditionally.
    fn body_to_bytes_stream(
        body: Self::Body,
    ) -> impl futures_util::Stream<
        Item = std::result::Result<Bytes, Box<dyn std::error::Error + Send + Sync>>,
    > + Send {
        use futures_util::StreamExt;
        http_body_util::BodyStream::new(body).filter_map(|frame_result| async move {
            match frame_result {
                Ok(frame) => frame
                    .into_data()
                    .ok()
                    .map(Ok::<Bytes, Box<dyn std::error::Error + Send + Sync>>),
                Err(e) => Some(Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)),
            }
        })
    }
}

impl PqcStreamingCacheManager {
    /// Read from memory tier only (no disk fallback).
    async fn read_mem(&self, cache_key: &str) -> Option<Bytes> {
        let mem = self.mem.as_ref()?;
        mem.get(cache_key).await.map(|arc| (*arc).clone())
    }

    /// One-shot lazy initialization of the byte counter. The sync
    /// UniFFI constructor can't spawn a tokio task, so the counter is
    /// seeded on the first cache call (put or get) instead — by then
    /// UniFFI's tokio runtime is live and we're already on a tokio
    /// worker. Idempotent and cheap after the first call.
    async fn ensure_seeded(&self) {
        if let Some(disk) = &self.disk {
            // Optimistic load: post-seed every cache call is one acquire load.
            if disk.seeded.load(Ordering::Acquire) {
                return;
            }
            // Serialize racing first-callers via evict_lock so we don't
            // double-walk the cacache index on cold start.
            let _guard = disk.evict_lock.lock().await;
            if disk.seeded.load(Ordering::Acquire) {
                return;
            }
            resync_from_disk(disk).await;
            disk.seeded.store(true, Ordering::Release);
        }
    }

    /// Evict oldest entries until under budget. Best-effort; called
    /// after each put. Serialized via evict_lock so concurrent puts
    /// don't all race the same rescan. With one cacache key per
    /// response, this is a flat oldest-first sweep — no pair
    /// aggregation, no half-pair orphan failure mode.
    async fn evict_if_over_budget(&self) {
        let disk = match &self.disk {
            Some(d) => d,
            None => return,
        };
        if disk.bytes.load(Ordering::Acquire) <= disk.max_bytes {
            return;
        }
        let _guard = disk.evict_lock.lock().await;
        // Authoritative resync inside the lock: handles counter drift
        // (e.g. an OS-side blob loss that left our counter high) and
        // the cold-start window before `ensure_seeded` has run.
        resync_from_disk(disk).await;
        if disk.bytes.load(Ordering::Acquire) <= disk.max_bytes {
            return;
        }

        // Walk in spawn_blocking — list_sync is a synchronous directory
        // traversal that would otherwise park a tokio worker for the
        // duration of the scan. Each item now corresponds to one logical
        // entry (one key per response), so the previous meta/body
        // aggregation pass is gone.
        let path = disk.path.clone();
        let entries: Vec<(String, cacache::Integrity, u64, u64)> =
            tokio::task::spawn_blocking(move || {
                let mut v: Vec<_> = cacache::list_sync(&path)
                    .flatten()
                    .map(|item| (item.key, item.integrity, item.size as u64, item.time as u64))
                    .collect();
                v.sort_by_key(|(_, _, _, t)| *t);
                v
            })
            .await
            .unwrap_or_default();

        let mut total = disk.bytes.load(Ordering::Acquire);
        for (key, integrity, size, _) in entries {
            if total <= disk.max_bytes {
                break;
            }
            // Drop the index entry AND its content blob — cacache::remove
            // drops only the key; remove_hash reclaims the blob.
            let _ = cacache::remove(&disk.path, &key).await;
            let _ = cacache::remove_hash(&disk.path, &integrity).await;
            total = total.saturating_sub(size);
        }
        disk.bytes.store(total, Ordering::Release);
    }

    /// Collect-then-write `put` path. Used for small known-length
    /// responses (≤ INLINE_BUFFERED_THRESHOLD) and for the
    /// mem-only-cache configuration (which has to materialize the body
    /// anyway). Larger bodies go through `put_tee` instead.
    async fn put_buffered<B>(
        &self,
        cache_key: String,
        parts: http::response::Parts,
        body: B,
        policy: CachePolicy,
        metadata: Option<Vec<u8>>,
    ) -> HttpCacheResult<Response<PqcCachedBody>>
    where
        B: HttpBody + Send + 'static,
        B::Data: Send,
        B::Error: Into<StreamingError>,
    {
        use http_body_util::BodyExt;
        let body_bytes = body
            .collect()
            .await
            .map_err(|e| StreamingError::new(e.into()))?
            .to_bytes();
        let body_size = body_bytes.len() as u64;

        // Disk tier: write iff within both the per-entry and total caps.
        if let Some(disk) = &self.disk {
            if body_size <= self.per_entry_disk && body_size <= disk.max_bytes {
                // Body size is known up-front for the buffered path, so
                // raw_metadata gets `body_size: Some(len)` directly — no
                // race window like put_tee.
                let meta_blob = serialize_cache_metadata(
                    parts.status,
                    parts.version,
                    &parts.headers,
                    policy,
                    metadata,
                    Some(body_size),
                )?;
                let mut writer = cacache::WriteOpts::new()
                    .raw_metadata(meta_blob)
                    .size(body_bytes.len())
                    .open(&disk.path, cache_key.clone())
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("writer create: {e}")))?;
                tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("body write: {e}")))?;
                writer
                    .commit()
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("commit: {e}")))?;
                disk.bytes.fetch_add(body_size, Ordering::AcqRel);
                self.evict_if_over_budget().await;
            }
        }

        // Memory tier: independent of the disk tier — mem-only configs
        // (cache_dir=None) must still cache; gated only on per_entry_mem.
        if let Some(mem) = &self.mem {
            if body_size <= self.per_entry_mem {
                mem.insert(cache_key.clone(), Arc::new(body_bytes.clone()))
                    .await;
            }
        }

        build_response_with_body(parts, PqcCachedBody::Buffered { data: body_bytes })
    }

    /// Tee `put` path — used when Content-Length is large-known or
    /// absent (chunked transfer-encoding). Spawns a background task
    /// that pulls frames from the upstream body and simultaneously:
    ///   - writes each chunk to `cacache::Writer` (disk side)
    ///   - sends each chunk through a bounded mpsc channel to the
    ///     consumer-side `StreamBody` (consumer side)
    ///
    /// Peak memory: one frame in flight (typically 16–64 KiB from
    /// reqwest/hyper) plus the channel buffer (TEE_CHANNEL_DEPTH × frame
    /// size, ≈ 1 MiB). Matches OkHttp's `CacheRequestBody` tee pattern.
    ///
    /// Failure modes (matched against OkHttp):
    ///  - Cache write fails mid-stream → writer dropped (cacache cleans
    ///    up tmp file), consumer continues reading. Best-effort cache.
    ///  - Consumer drops response → channel send fails, task exits,
    ///    writer dropped (tmp cleaned).
    ///  - Upstream body errors → error forwarded to consumer, writer
    ///    dropped (no cache commit).
    ///  - Body grows past per_entry_disk → drop writer mid-stream,
    ///    continue forwarding to consumer.
    async fn put_tee<B>(
        &self,
        cache_key: String,
        parts: http::response::Parts,
        body: B,
        policy: CachePolicy,
        metadata: Option<Vec<u8>>,
    ) -> HttpCacheResult<Response<PqcCachedBody>>
    where
        B: HttpBody + Send + 'static,
        B::Data: Send,
        B::Error: Into<StreamingError>,
    {
        let disk = self
            .disk
            .as_ref()
            .expect("put_tee must only be invoked when disk tier is present");

        // Serialize the initial metadata blob with `body_size: None` —
        // the sentinel for "tee write in progress". A concurrent get()
        // sees this and treats the entry as a miss (falls through to
        // network) rather than reading an empty body. Once the upstream
        // EOFs and `total` is known, the post-commit index re-insert
        // overwrites raw_metadata with `body_size: Some(total)`. If
        // that reinsert ever fails, the entry stays at `None` forever
        // → gets keep missing → consumer refetches. No empty-body
        // failure mode. Validates synchronously so a malformed
        // CachePolicy fails BEFORE we hand the consumer a body the
        // background task can't complete. Headers/policy/metadata are
        // cloned (cheap — Vec<u8> + HashMap + Vec<u8>) so the task can
        // re-serialize with body_size=Some(total) on commit.
        let meta_blob = serialize_cache_metadata(
            parts.status,
            parts.version,
            &parts.headers,
            policy.clone(),
            metadata.clone(),
            None,
        )?;
        let task_parts_headers = parts.headers.clone();
        let task_status = parts.status;
        let task_version = parts.version;
        let task_policy = policy.clone();
        let task_metadata = metadata.clone();

        // Snapshot of disk-tier state for the spawned task — we can't
        // hold &self across the spawn boundary.
        let disk_path = disk.path.clone();
        let disk_bytes = Arc::clone(&disk.bytes);
        let per_entry_disk = self.per_entry_disk;
        let per_entry_mem = self.per_entry_mem;
        let mem = self.mem.clone();
        let manager = self.clone(); // for post-commit eviction trigger
        let task_cache_key = cache_key.clone();

        // Map B::Data → Bytes (via Buf::copy_to_bytes, one alloc per
        // frame — typically 16-64 KiB upstream, fine on mobile) and
        // B::Error → StreamingError BEFORE spawning. After this both
        // are Send + 'static so the body moves into the task without
        // pushing Send/'static bounds up into the trait `put`
        // signature (which would force them on every caller).
        use bytes::Buf;
        use http_body_util::BodyExt;
        let body_for_task = body
            .map_err(Into::<StreamingError>::into)
            .map_frame(|frame| {
                frame.map_data(|mut d| {
                    let len = d.remaining();
                    d.copy_to_bytes(len)
                })
            });

        let (tx, rx) =
            tokio::sync::mpsc::channel::<Result<Frame<Bytes>, StreamingError>>(TEE_CHANNEL_DEPTH);

        tokio::spawn(async move {
            let mut writer = match cacache::WriteOpts::new()
                .raw_metadata(meta_blob)
                .open(&disk_path, task_cache_key.clone())
                .await
            {
                Ok(w) => Some(w),
                Err(e) => {
                    log::warn!("pqc cache: tee writer open failed: {e}");
                    None
                }
            };

            let mut body = Box::pin(body_for_task);
            let mut total: u64 = 0;
            // Mem-tier accumulator — bounded by per_entry_mem so chunked
            // bodies that turn out to be small can still hit the mem
            // tier on subsequent gets. Dropped if total grows past the
            // cap, so the buffer never exceeds per_entry_mem.
            let mut mem_buf: Option<BytesMut> = if mem.is_some() && per_entry_mem > 0 {
                Some(BytesMut::with_capacity(STREAM_CHUNK_SIZE))
            } else {
                None
            };

            loop {
                let frame_opt = body.as_mut().frame().await;
                let Some(frame_res) = frame_opt else { break };
                let frame = match frame_res {
                    Ok(f) => f,
                    Err(e) => {
                        // Upstream errored — forward to consumer, abort
                        // cache write by dropping the writer.
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };

                if !frame.is_data() {
                    // Trailers / unknown frame types — forward as-is,
                    // don't write to cache.
                    if tx.send(Ok(frame)).await.is_err() {
                        return;
                    }
                    continue;
                }

                let data = match frame.into_data() {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                total += data.len() as u64;

                // Body grew past the disk per-entry cap — stop caching
                // (drop the writer) but keep forwarding to consumer.
                if writer.is_some() && total > per_entry_disk {
                    writer = None;
                    mem_buf = None;
                }

                // Mem accumulator tracking.
                if let Some(buf) = mem_buf.as_mut() {
                    if (buf.len() as u64 + data.len() as u64) > per_entry_mem {
                        mem_buf = None;
                    } else {
                        buf.extend_from_slice(&data);
                    }
                }

                // Send to consumer FIRST (low latency); the disk write
                // below happens after the consumer has the bytes. If
                // consumer dropped, channel send fails — abort cache
                // too. data.clone() is a ref-bump on Bytes (O(1)).
                let send_result = tx.send(Ok(Frame::data(data.clone()))).await;
                if send_result.is_err() {
                    return;
                }

                // Write to cache. On failure, abort cache write but
                // keep forwarding to consumer (matches OkHttp's
                // best-effort cache semantics — consumer shouldn't
                // fail because disk filled up).
                if let Some(w) = writer.as_mut() {
                    if let Err(e) = tokio::io::AsyncWriteExt::write_all(w, &data).await {
                        log::warn!("pqc cache: tee write_all failed: {e}");
                        writer = None;
                        mem_buf = None;
                    }
                }
            }

            // Upstream EOF — commit if we still have a writer.
            if let Some(w) = writer {
                let integrity = match w.commit().await {
                    Ok(sri) => sri,
                    Err(e) => {
                        log::warn!("pqc cache: tee commit failed: {e}");
                        return;
                    }
                };
                // The commit above wrote the index entry with size=0
                // AND raw_metadata's body_size = None (the in-flight
                // sentinel). Now that `total` is known, re-serialize
                // raw_metadata with `body_size: Some(total)` and
                // reinsert with the correct cacache `size` — this is
                // what makes get() deliver the full body (read off
                // body_size, not entry.size — see the body_size guard
                // in get()) and what makes eviction's list_sync see
                // the real on-disk footprint. Uses the same
                // serialize_cache_metadata helper as the initial
                // commit + put_buffered, so a future CacheMetadata
                // field change can't silently miss the commit path.
                let final_meta_blob = match serialize_cache_metadata(
                    task_status,
                    task_version,
                    &task_parts_headers,
                    task_policy,
                    task_metadata,
                    Some(total),
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        log::warn!("pqc cache: tee final meta serialize failed: {e}");
                        return;
                    }
                };
                // Clone the integrity before reinsert (which consumes
                // it via .integrity()) so we can use it for blob
                // cleanup if the reinsert fails.
                let integrity_for_cleanup = integrity.clone();
                let reinsert_opts = cacache::WriteOpts::new()
                    .raw_metadata(final_meta_blob)
                    .size(total as usize)
                    .integrity(integrity);
                if let Err(e) =
                    cacache::index::insert_async(&disk_path, &task_cache_key, reinsert_opts).await
                {
                    log::warn!("pqc cache: tee index re-insert failed: {e}");
                    // Without this cleanup, the body blob committed
                    // above stays on disk forever — its index entry
                    // has size=0 (never accumulated into disk.bytes,
                    // so evict_if_over_budget can't see it) and the
                    // content blob is unreferenced after the failed
                    // reinsert. cacache::remove + remove_hash reclaim
                    // both; failures are logged but not fatal (best-
                    // effort cleanup on an already-degraded path).
                    let _ = cacache::remove(&disk_path, &task_cache_key).await;
                    let _ = cacache::remove_hash(&disk_path, &integrity_for_cleanup).await;
                    return;
                }
                disk_bytes.fetch_add(total, Ordering::AcqRel);
                if let (Some(buf), Some(mem_tier)) = (mem_buf, mem) {
                    if total <= per_entry_mem {
                        mem_tier
                            .insert(task_cache_key, Arc::new(buf.freeze()))
                            .await;
                    }
                }
                // Best-effort eviction in case this commit pushed the
                // total over budget. Held off until after commit so a
                // racing get() during the stream doesn't miss.
                manager.evict_if_over_budget().await;
            }
        });

        // Return a response whose body streams from the channel. The
        // ReceiverFrameStream wraps the Receiver; StreamBody adapts it
        // to http_body::Body; Passthrough delegates poll_frame to it.
        let receiver_stream = ReceiverFrameStream { rx };
        let stream_body = http_body_util::StreamBody::new(receiver_stream);
        build_response_with_body(
            parts,
            PqcCachedBody::Passthrough {
                body: Box::pin(stream_body),
            },
        )
    }
}

/// Single source of truth for the on-disk `CacheMetadata` postcard
/// shape. Called by `put_buffered` (with body_size known up front),
/// `put_tee` initial commit (body_size=None, the in-flight sentinel),
/// and `put_tee` post-commit reinsert (body_size=Some(total)). Takes
/// the constituent fields (not `&Parts`) so the tee task's commit
/// path can call it from inside the spawn without needing to thread
/// the whole `Parts` struct across the spawn boundary.
fn serialize_cache_metadata(
    status: http::StatusCode,
    version: http::Version,
    headers: &http::HeaderMap,
    policy: CachePolicy,
    user_metadata: Option<Vec<u8>>,
    body_size: Option<u64>,
) -> HttpCacheResult<Vec<u8>> {
    let headers: Vec<(String, Vec<u8>)> = headers
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_owned()))
        .collect();
    let meta = CacheMetadata {
        status: status.as_u16(),
        version: version_to_u8(version),
        headers,
        policy,
        user_metadata,
        body_size,
    };
    let blob = postcard::to_allocvec(&meta)
        .map_err(|e| HttpCacheError::cache(format!("meta serialize: {e}")))?;
    Ok(blob)
}

/// Adapts a tokio mpsc `Receiver<Result<Frame<Bytes>, StreamingError>>`
/// to the `Stream` shape `http_body_util::StreamBody` expects. Just a
/// thin pass-through over `Receiver::poll_recv` — used only by put_tee.
struct ReceiverFrameStream {
    rx: tokio::sync::mpsc::Receiver<Result<Frame<Bytes>, StreamingError>>,
}

impl futures_core::Stream for ReceiverFrameStream {
    type Item = Result<Frame<Bytes>, StreamingError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Build the in-memory LRU tier. Returns `None` when `cap` is 0 (the
/// consumer-controlled opt-out for OkHttp-style disk-only behavior).
/// Platform-agnostic: OkHttp's disk-only design was a product choice
/// driven by `Cache` being a `final` class and historical Dalvik heap
/// caps, not a limitation of Android.
fn build_mem_tier(cap: u64) -> Option<moka::future::Cache<String, Arc<Bytes>>> {
    if cap == 0 {
        return None;
    }
    Some(
        moka::future::Cache::builder()
            .max_capacity(cap)
            .weigher(|_k: &String, v: &Arc<Bytes>| v.len().try_into().unwrap_or(u32::MAX))
            .build(),
    )
}

/// Authoritative resync of the byte counter from the cacache index.
/// Sums every live entry and stores the total in `disk.bytes`. Called
/// from `clear`, the cold-start path of `ensure_seeded`, and the top
/// of `evict_if_over_budget` to self-heal drift. Wrapped in
/// `spawn_blocking` so cacache's sync index walk doesn't park a tokio
/// worker (small caches: microseconds; populated 20 MiB cache with
/// ~1k entries: tens of ms).
///
/// Caller must hold `disk.evict_lock` for the duration — the final
/// `store` overwrites any concurrent `put.fetch_add` racing this scan,
/// and the lock both serializes resync callers and excludes the puts
/// that would otherwise be lost.
async fn resync_from_disk(disk: &DiskTier) {
    let path = disk.path.clone();
    let total = tokio::task::spawn_blocking(move || {
        cacache::list_sync(&path)
            .flatten()
            .map(|i| i.size as u64)
            .sum::<u64>()
    })
    .await
    .unwrap_or(0);
    disk.bytes.store(total, Ordering::Release);
}

/// Remove a single cache entry from disk: drop the index key AND its
/// content blob (by integrity). Returns the body bytes reclaimed so the
/// caller can decrement the counter.
///
/// `cacache::remove` drops only the key — the content-addressable blob
/// lives until no key references its integrity. We follow up with
/// `remove_hash` to reclaim the blob; without it, RFC 9111
/// invalidations (delete on every unsafe-method match against a cached
/// GET) leak storage indefinitely.
///
/// Caveat: cacache deduplicates blobs across keys by content integrity.
/// If two cache entries happen to have byte-identical bodies, they
/// share one blob; removing one's blob via `remove_hash` orphans the
/// other entry's body. For HTTP responses keyed by URL+Vary this is
/// rare in practice — distinct URLs almost never produce identical
/// response bytes — and the alternative (never calling `remove_hash`)
/// is the orphan-blob leak we just fixed. cacache 13 exposes no
/// reference-counting primitive to disambiguate.
async fn remove_entry(disk: &DiskTier, cache_key: &str) -> u64 {
    let entry = match cacache::metadata(&disk.path, cache_key).await {
        Ok(Some(m)) => m,
        _ => return 0,
    };
    let reclaimed = entry.size as u64;
    let _ = cacache::remove(&disk.path, cache_key).await;
    let _ = cacache::remove_hash(&disk.path, &entry.integrity).await;
    reclaimed
}

/// Per-request slot for capturing the real response URL before the cache
/// layer drops it (see [`capture_response_url`]). `client.rs` attaches one
/// via request extensions and reads it back after `send()`.
#[derive(Clone, Default)]
pub struct UrlSlot(Arc<std::sync::Mutex<Option<String>>>);

impl UrlSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// The captured URL, if the middleware ran. Empty on a cache HIT, where
    /// `StreamingCache` short-circuits before reaching it — so a HIT of a
    /// cacheable redirected response reports the request URL, unlike native
    /// caches, which retain the post-redirect URL.
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.take())
    }

    fn set(&self, url: String) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(url);
        }
    }
}

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Middleware recording the real (post-redirect) response URL into the
/// request's [`UrlSlot`], when it carries one.
///
/// Why: the streaming cache layer round-trips responses through
/// `http::Response`, which drops reqwest's `ResponseUrl` extension; reqwest
/// then substitutes `http://no.url.provided.local`, so `Response::url()` is a
/// placeholder for every cached-backend request — even non-cacheable ones.
/// Installed INNERMOST (after `StreamingCache`) so `next.run` reaches reqwest
/// directly, where `res.url()` is still real. A plain `fn` so the blanket
/// `Fn` impl of `Middleware` applies. No upstream fix as of http-cache
/// 1.0.0-alpha.6 — re-audit on any `http-cache`/`reqwest` bump.
fn capture_response_url<'a>(
    req: reqwest::Request,
    ext: &'a mut http::Extensions,
    next: reqwest_middleware::Next<'a>,
) -> BoxFuture<'a, reqwest_middleware::Result<reqwest::Response>> {
    let slot = ext.get::<UrlSlot>().cloned();
    Box::pin(async move {
        let res = next.run(req, ext).await?;
        if let Some(slot) = slot {
            slot.set(res.url().to_string());
        }
        Ok(res)
    })
}

/// Build the streaming-aware reqwest cache middleware from this manager.
/// Used by `client.rs::new()` to wrap the reqwest client.
pub fn build_cached_client(
    client: reqwest::Client,
    manager: PqcStreamingCacheManager,
) -> reqwest_middleware::ClientWithMiddleware {
    use http_cache::CacheMode;
    use http_cache_reqwest::StreamingCache;
    reqwest_middleware::ClientBuilder::new(client)
        .with(StreamingCache::new(manager, CacheMode::Default))
        // Innermost (after the cache) so it still sees reqwest's real URL.
        .with(capture_response_url)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RedirectPolicy;

    /// Construct a config with a temp `cache_dir` so the disk tier
    /// builds without polluting the real ~/.cache directory. The mem
    /// tier follows the user-default (Some(4 MiB)).
    fn tmp_config(dir: &std::path::Path) -> PqcConfig {
        PqcConfig {
            pinned_cert_sha256: vec![],
            default_timeout_ms: None,
            connect_timeout_ms: None,
            read_idle_timeout_ms: None,
            enable_cookies: false,
            user_agent: None,
            redirect_policy: RedirectPolicy::SameOriginOnly {},
            dns_resolver: None,
            proxy_url: None,
            max_inflight_total: Some(64),
            max_inflight_per_host: Some(5),
            enable_cache: true,
            cache_dir: Some(dir.to_string_lossy().into_owned()),
            max_cache_bytes: Some(1024 * 1024),      // 1 MiB
            max_memory_cache_bytes: Some(64 * 1024), // 64 KiB
        }
    }

    /// Direct-write a body + metadata entry so get/delete tests don't
    /// have to construct full Response<B> values. Bypasses the
    /// cacheability gate but exercises the exact single-key layout
    /// that `get` reads.
    async fn write_entry(m: &PqcStreamingCacheManager, key: &str, body: &[u8]) {
        write_entry_with_headers(m, key, body, vec![]).await;
    }

    /// Same as `write_entry` but with caller-supplied stored headers.
    /// Used by the dedup regression test to seed a stale x-pqc-cache-hit
    /// without re-implementing the whole cacache write path.
    async fn write_entry_with_headers(
        m: &PqcStreamingCacheManager,
        key: &str,
        body: &[u8],
        headers: Vec<(String, Vec<u8>)>,
    ) {
        let disk = m.disk.as_ref().expect("disk tier present");
        let now = std::time::SystemTime::UNIX_EPOCH;
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let resp = http::Response::builder().status(200).body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &resp.into_parts().0,
            now,
            http_cache_semantics::CacheOptions::default(),
        );
        let meta = CacheMetadata {
            status: 200,
            version: 11,
            headers,
            policy,
            user_metadata: None,
            body_size: Some(body.len() as u64),
        };
        let blob = postcard::to_allocvec(&meta).unwrap();

        let mut writer = cacache::WriteOpts::new()
            .raw_metadata(blob)
            .size(body.len())
            .open(&disk.path, key)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, body)
            .await
            .unwrap();
        writer.commit().await.unwrap();
        disk.bytes.fetch_add(body.len() as u64, Ordering::AcqRel);
    }

    /// Drain a `PqcCachedBody` to bytes for assertion.
    async fn drain(mut body: PqcCachedBody) -> Vec<u8> {
        use http_body_util::BodyExt;
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Some(data) = frame.data_ref() {
                out.extend_from_slice(data);
            }
            // Consume the frame.
            let _ = frame.into_data();
        }
        out
    }

    #[tokio::test]
    async fn new_returns_none_when_both_tiers_disabled() {
        let mut cfg = PqcConfig {
            pinned_cert_sha256: vec![],
            default_timeout_ms: None,
            connect_timeout_ms: None,
            read_idle_timeout_ms: None,
            enable_cookies: false,
            user_agent: None,
            redirect_policy: RedirectPolicy::SameOriginOnly {},
            dns_resolver: None,
            proxy_url: None,
            max_inflight_total: None,
            max_inflight_per_host: None,
            enable_cache: true,
            cache_dir: None,
            max_cache_bytes: None,
            max_memory_cache_bytes: Some(0),
        };
        assert!(PqcStreamingCacheManager::new(&cfg).is_none());
        // Mem-only is still a valid manager.
        cfg.max_memory_cache_bytes = Some(1024);
        assert!(PqcStreamingCacheManager::new(&cfg).is_some());
    }

    #[tokio::test]
    async fn get_streams_from_disk_after_write() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let body = b"hello cached world".to_vec();
        write_entry(&m, "k1", &body).await;
        let (resp, _policy) = m.get("k1").await.unwrap().unwrap();
        assert_eq!(resp.status(), 200);
        let drained = drain(resp.into_body()).await;
        assert_eq!(drained, body);
    }

    /// `x-pqc-cache-hit: true` on every hit (mem-tier or disk-tier),
    /// single-valued.
    #[tokio::test]
    async fn get_sets_x_pqc_cache_hit_true_on_hit() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        write_entry(&m, "k1", b"hit body").await;
        let (resp, _policy) = m.get("k1").await.unwrap().unwrap();
        let all: Vec<_> = resp.headers().get_all("x-pqc-cache-hit").iter().collect();
        assert_eq!(all.len(), 1, "must be single-valued, got {all:?}");
        assert_eq!(all[0].to_str().unwrap(), "true");
    }

    /// Duplicate-header regression: when CacheMetadata.headers already
    /// contains an `x-pqc-cache-hit` (which could happen on a future
    /// 304-revalidation re-put, or if an upstream ever sends our
    /// header), `response_head` must NOT append a second value.
    #[tokio::test]
    async fn get_does_not_duplicate_x_pqc_cache_hit_when_stored() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        write_entry_with_headers(
            &m,
            "k1",
            b"dedup body",
            vec![("x-pqc-cache-hit".to_string(), b"stale-value".to_vec())],
        )
        .await;

        let (resp, _policy) = m.get("k1").await.unwrap().unwrap();
        let all: Vec<_> = resp.headers().get_all("x-pqc-cache-hit").iter().collect();
        assert_eq!(all.len(), 1, "must be single-valued, got {all:?}");
        assert_eq!(all[0].to_str().unwrap(), "true");
    }

    #[tokio::test]
    async fn get_returns_none_on_corrupt_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let disk = m.disk.as_ref().unwrap();
        // Write an entry whose raw_metadata is garbage — postcard will
        // fail to deserialize and we treat that as a cache miss
        // (matches upstream's #141 fix).
        let body = b"body";
        let mut writer = cacache::WriteOpts::new()
            .raw_metadata(vec![0xff, 0xff, 0xff, 0xff])
            .size(body.len())
            .open(&disk.path, "k1")
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, body)
            .await
            .unwrap();
        writer.commit().await.unwrap();
        assert!(m.get("k1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_invalidates_both_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        write_entry(&m, "k1", b"v1").await;
        // Also seed the mem tier directly so we can verify delete
        // touches both sides.
        m.mem
            .as_ref()
            .unwrap()
            .insert("k1".to_string(), Arc::new(Bytes::from_static(b"v1")))
            .await;
        m.delete("k1").await.unwrap();
        // After delete the next get must miss on both tiers.
        assert!(m.get("k1").await.unwrap().is_none());
        assert!(m.mem.as_ref().unwrap().get("k1").await.is_none());
    }

    #[tokio::test]
    async fn clear_empties_both_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        write_entry(&m, "k1", b"v1").await;
        write_entry(&m, "k2", b"v2longer").await;
        m.mem
            .as_ref()
            .unwrap()
            .insert("k1".to_string(), Arc::new(Bytes::from_static(b"v1")))
            .await;
        assert!(m.size().await > 0);
        m.clear().await;
        assert_eq!(m.size().await, 0);
        assert!(m.get("k1").await.unwrap().is_none());
        assert!(m.get("k2").await.unwrap().is_none());
    }

    /// Regression for the Buffered single-chunk bug: a body larger than
    /// STREAM_CHUNK_SIZE must be delivered in multiple frames, not one.
    /// Otherwise read_chunk() on a cache-miss path hands the consumer a
    /// single huge Vec<u8>, defeating the streaming feature.
    #[tokio::test]
    async fn buffered_body_chunks_match_stream_size() {
        use http_body_util::BodyExt;
        let big = Bytes::from(vec![0xab; STREAM_CHUNK_SIZE * 3 + 17]);
        let mut body = PqcCachedBody::Buffered { data: big.clone() };
        let mut frames = 0usize;
        let mut max_chunk = 0usize;
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Some(data) = frame.data_ref() {
                frames += 1;
                max_chunk = max_chunk.max(data.len());
                assert!(data.len() <= STREAM_CHUNK_SIZE);
            }
        }
        // 3 full chunks + 1 partial = 4 frames.
        assert_eq!(frames, 4);
        assert_eq!(max_chunk, STREAM_CHUNK_SIZE);
    }

    /// Regression for the memory-only cache no-op: a config with
    /// cache_dir=None + max_memory_cache_bytes=Some must actually serve
    /// mem-tier hits. The previous wiring nested mem.insert inside
    /// `if cacheable`, which evaluated false whenever the disk tier
    /// was absent — so mem-only was silently disabled.
    #[tokio::test]
    async fn memory_only_cache_actually_stores() {
        let cfg = PqcConfig {
            pinned_cert_sha256: vec![],
            default_timeout_ms: None,
            connect_timeout_ms: None,
            read_idle_timeout_ms: None,
            enable_cookies: false,
            user_agent: None,
            redirect_policy: RedirectPolicy::SameOriginOnly {},
            dns_resolver: None,
            proxy_url: None,
            max_inflight_total: None,
            max_inflight_per_host: None,
            enable_cache: true,
            cache_dir: None,
            max_cache_bytes: None,
            max_memory_cache_bytes: Some(64 * 1024),
        };
        let m = PqcStreamingCacheManager::new(&cfg).expect("mem-only manager builds");
        // No disk tier; assert we can still write to the mem tier and
        // read it back via the private read_mem helper.
        assert!(m.disk.is_none());
        let mem = m.mem.as_ref().expect("mem tier present");
        mem.insert("k".to_string(), Arc::new(Bytes::from_static(b"hello")))
            .await;
        assert_eq!(m.read_mem("k").await.as_deref(), Some(&b"hello"[..]));
    }

    /// Eviction drops oldest entries until the byte counter is back
    /// under budget. With one cacache key per response, the only
    /// invariant to assert is that the entries left on disk fit.
    ///
    /// NOTE: distinct body bytes per entry are required — cacache is
    /// content-addressable, so two entries with byte-identical bodies
    /// share one content blob via integrity; removing one's blob via
    /// `remove_hash` also removes the other. In production HTTP usage
    /// this is rare (different URLs almost never produce byte-identical
    /// responses), and the alternative — leaving every blob behind —
    /// is the orphan-blob bug we just fixed.
    #[tokio::test]
    async fn eviction_drops_oldest_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = tmp_config(dir.path());
        cfg.max_cache_bytes = Some(1024); // tight enough that 2 entries trip it
        let m = PqcStreamingCacheManager::new(&cfg).unwrap();
        write_entry(&m, "k1", &vec![0xaa; 600]).await;
        // Tiny pause so the cacache timestamps differ — eviction sorts
        // by time ascending; without this both entries can share the
        // same millisecond and either may be evicted first.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        write_entry(&m, "k2", &vec![0xbb; 600]).await;
        m.evict_if_over_budget().await;
        // At least one entry must have been evicted; counter back under.
        assert!(
            m.size().await <= 1024,
            "size {} should be <= max_bytes 1024 after evict",
            m.size().await
        );
        // The oldest (k1) is the one evicted; k2 survives.
        assert!(m.get("k1").await.unwrap().is_none(), "k1 should be gone");
        assert!(m.get("k2").await.unwrap().is_some(), "k2 should remain");
    }

    /// Regression for the Content-Length pre-check bypass. A response
    /// whose advertised Content-Length is too big for every tier MUST
    /// NOT get buffered — it returns a Passthrough body and never
    /// touches the cacache index or the mem tier.
    #[tokio::test]
    async fn oversized_content_length_bypasses_cache() {
        use http::header::CONTENT_LENGTH;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = tmp_config(dir.path());
        cfg.max_cache_bytes = Some(1024 * 1024); // 1 MiB → per_entry_disk = 50 KiB
        cfg.max_memory_cache_bytes = Some(64 * 1024); // → per_entry_mem ≈ 3.2 KiB
        let m = PqcStreamingCacheManager::new(&cfg).unwrap();

        // Construct a Response<Full<Bytes>> claiming a 5 MiB body
        // via Content-Length. The actual body Full is small (10 bytes)
        // but the header is what triggers the pre-check.
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"0123456789"));
        let response = http::Response::builder()
            .status(200)
            .header(CONTENT_LENGTH, "5242880")
            .body(body)
            .unwrap();
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &response.into_parts().0,
            std::time::SystemTime::UNIX_EPOCH,
            http_cache_semantics::CacheOptions::default(),
        );
        // Rebuild Response (we consumed the original via into_parts).
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"0123456789"));
        let response = http::Response::builder()
            .status(200)
            .header(CONTENT_LENGTH, "5242880")
            .body(body)
            .unwrap();

        let url = "http://example.test/".parse::<http_cache::Url>().unwrap();
        let result = m
            .put("oversized".to_string(), response, policy, url, None)
            .await
            .expect("put should succeed via passthrough");

        // The body we got back is Passthrough (not Buffered) — assert
        // by checking that nothing was written to cacache.
        let disk = m.disk.as_ref().unwrap();
        let entry = cacache::metadata(&disk.path, "oversized")
            .await
            .ok()
            .flatten();
        assert!(
            entry.is_none(),
            "oversized response with known Content-Length should not be cached"
        );
        // Counter should be untouched.
        assert_eq!(m.size().await, 0);
        // Mem tier should also be empty.
        assert!(m.read_mem("oversized").await.is_none());
        // Body should still be drainable (it's Passthrough).
        use http_body_util::BodyExt;
        let drained = result.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&drained[..], b"0123456789");
    }

    /// Small-body path is unchanged: a body that fits in per_entry_disk
    /// still goes through the buffered+caching codepath. Guards that
    /// the bypass didn't accidentally skip the common case.
    #[tokio::test]
    async fn small_content_length_still_caches() {
        use http::header::CONTENT_LENGTH;
        let dir = tempfile::tempdir().unwrap();
        let cfg = tmp_config(dir.path()); // 1 MiB / 64 KiB defaults
        let m = PqcStreamingCacheManager::new(&cfg).unwrap();

        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"hi"));
        let response = http::Response::builder()
            .status(200)
            .header(CONTENT_LENGTH, "2")
            .body(body)
            .unwrap();
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &response.into_parts().0,
            std::time::SystemTime::UNIX_EPOCH,
            http_cache_semantics::CacheOptions::default(),
        );
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"hi"));
        let response = http::Response::builder()
            .status(200)
            .header(CONTENT_LENGTH, "2")
            .body(body)
            .unwrap();
        let url = "http://example.test/".parse::<http_cache::Url>().unwrap();
        let _ = m
            .put("small".to_string(), response, policy, url, None)
            .await
            .unwrap();

        // Small body — disk got written.
        let disk = m.disk.as_ref().unwrap();
        let entry = cacache::metadata(&disk.path, "small")
            .await
            .unwrap()
            .expect("small response should be cached on disk");
        assert_eq!(entry.size, 2);
    }

    /// Build a Full<Bytes> response + matching CachePolicy for the put
    /// tests. Constructs the response once, snapshots parts for the
    /// policy, and reassembles via from_parts so there's no drift
    /// between the policy view and the actual put payload.
    fn miss_test_response(
        cl: Option<&str>,
        body_bytes: &'static [u8],
    ) -> (http::Response<http_body_util::Full<Bytes>>, CachePolicy) {
        use http::header::CONTENT_LENGTH;
        let mut b = http::Response::builder().status(200);
        if let Some(v) = cl {
            b = b.header(CONTENT_LENGTH, v);
        }
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(body_bytes));
        let (parts, body) = b.body(body).unwrap().into_parts();
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &parts,
            std::time::SystemTime::UNIX_EPOCH,
            http_cache_semantics::CacheOptions::default(),
        );
        (http::Response::from_parts(parts, body), policy)
    }

    fn assert_miss_header(resp: &Response<PqcCachedBody>) {
        assert_eq!(
            resp.headers()
                .get("x-pqc-cache-hit")
                .map(|v| v.to_str().unwrap()),
            Some("false"),
        );
    }

    /// put_buffered → small known-length body → `x-pqc-cache-hit: false`
    #[tokio::test]
    async fn put_buffered_sets_x_pqc_cache_hit_false() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let url = "http://example.test/".parse::<http_cache::Url>().unwrap();
        let (response, policy) = miss_test_response(Some("2"), b"hi");
        let resp = m
            .put("k".to_string(), response, policy, url, None)
            .await
            .unwrap();
        assert_miss_header(&resp);
    }

    /// put_tee → no Content-Length (chunked) → also `false`
    #[tokio::test]
    async fn put_tee_sets_x_pqc_cache_hit_false() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let url = "http://example.test/".parse::<http_cache::Url>().unwrap();
        // streaming_response: no Content-Length → routes through put_tee.
        let resp = streaming_response(4, 64, 0xa5);
        let (parts, body) = resp.into_parts();
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &parts,
            std::time::SystemTime::UNIX_EPOCH,
            http_cache_semantics::CacheOptions::default(),
        );
        let resp = http::Response::from_parts(parts, body);
        let out = m
            .put("k".to_string(), resp, policy, url, None)
            .await
            .unwrap();
        assert_miss_header(&out);
    }

    /// convert_body (middleware's skip-cache hook) — also `false`.
    #[tokio::test]
    async fn convert_body_sets_x_pqc_cache_hit_false() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"x"));
        let response = http::Response::builder().status(200).body(body).unwrap();
        let out = m.convert_body(response).await.unwrap();
        assert_miss_header(&out);
    }

    /// Miss-path dedup: if the upstream response parts already carry
    /// `x-pqc-cache-hit` (an upstream/proxy that happens to send our
    /// header), `build_response_with_body` must NOT append a second
    /// value next to ours.
    #[tokio::test]
    async fn build_response_does_not_duplicate_x_pqc_cache_hit_from_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let body = http_body_util::Full::<Bytes>::new(Bytes::from_static(b"x"));
        let response = http::Response::builder()
            .status(200)
            .header("x-pqc-cache-hit", "stale-upstream-value")
            .body(body)
            .unwrap();
        // convert_body funnels through passthrough_response →
        // build_response_with_body, the function under test.
        let out = m.convert_body(response).await.unwrap();
        let all: Vec<_> = out.headers().get_all("x-pqc-cache-hit").iter().collect();
        assert_eq!(all.len(), 1, "must be single-valued, got {all:?}");
        assert_eq!(all[0].to_str().unwrap(), "false");
    }

    #[tokio::test]
    async fn per_entry_caps_scale_with_total() {
        // 5% rule: a 200 MiB disk total should give ~10 MiB per-entry
        // disk cap. Verifies the rule is wired correctly and not a
        // hardcoded constant.
        assert_eq!(per_entry_disk_cap(200 * 1024 * 1024), 10 * 1024 * 1024);
        assert_eq!(per_entry_mem_cap(4 * 1024 * 1024), 4 * 1024 * 1024 / 20);
        // Zero divides cleanly to zero (no panic).
        assert_eq!(per_entry_disk_cap(0), 0);
        assert_eq!(per_entry_mem_cap(0), 0);
    }

    // ---- Tee-path regression tests (put_tee, chunked encoding) ----

    /// Build a streaming response body of N frames of `chunk_size` each
    /// with byte pattern `fill`. No Content-Length header → routes
    /// through put_tee.
    #[allow(clippy::type_complexity)] // test helper, type alias not worth it
    fn streaming_response(
        n_frames: usize,
        chunk_size: usize,
        fill: u8,
    ) -> http::Response<
        http_body_util::StreamBody<
            futures_util::stream::Iter<std::vec::IntoIter<Result<Frame<Bytes>, std::io::Error>>>,
        >,
    > {
        let frames: Vec<Result<Frame<Bytes>, std::io::Error>> = (0..n_frames)
            .map(|_| Ok(Frame::data(Bytes::from(vec![fill; chunk_size]))))
            .collect();
        let stream = futures_util::stream::iter(frames);
        let body = http_body_util::StreamBody::new(stream);
        http::Response::builder().status(200).body(body).unwrap()
    }

    fn policy_for(response: &http::Response<impl Sized>) -> CachePolicy {
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let (req_parts, _) = req.into_parts();
        // Build a fresh response_parts from status/headers so we don't
        // consume the test's response.
        let mut resp_parts = http::Response::new(()).into_parts().0;
        resp_parts.status = response.status();
        for (k, v) in response.headers().iter() {
            resp_parts.headers.insert(k, v.clone());
        }
        CachePolicy::new_options(
            &req_parts,
            &resp_parts,
            std::time::SystemTime::UNIX_EPOCH,
            http_cache_semantics::CacheOptions::default(),
        )
    }

    /// Tee path: chunked response (no Content-Length, > 64 KiB) caches
    /// on disk AND delivers all bytes to the consumer.
    #[tokio::test]
    async fn tee_chunked_response_caches_and_delivers() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = tmp_config(dir.path());
        // 4 MiB disk → per_entry_disk ≈ 200 KiB, room for our 160 KiB body.
        cfg.max_cache_bytes = Some(4 * 1024 * 1024);
        let m = PqcStreamingCacheManager::new(&cfg).unwrap();
        // 5 frames × 32 KiB = 160 KiB; no Content-Length, so the
        // dispatcher routes to put_tee.
        let n_frames = 5;
        let chunk_size = 32 * 1024;
        let response = streaming_response(n_frames, chunk_size, 0xaa);
        let policy = policy_for(&response);
        let url = "http://example.test/big"
            .parse::<http_cache::Url>()
            .unwrap();

        let result = m
            .put("big".to_string(), response, policy, url, None)
            .await
            .expect("tee put should succeed");

        // Drain the consumer-side body.
        use http_body_util::BodyExt;
        let drained = result.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(drained.len(), n_frames * chunk_size);
        assert!(drained.iter().all(|&b| b == 0xaa));

        // The background task may finish writing slightly after put()
        // returns. Wait for the commit to land before asserting disk
        // state — the channel-close happens on the upstream EOF, but
        // the commit is the very last thing the task does.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(_)) = cacache::metadata(dir.path(), "big").await {
                break;
            }
        }
        let entry = cacache::metadata(dir.path(), "big")
            .await
            .unwrap()
            .expect("tee put should have committed");
        assert_eq!(entry.size as usize, n_frames * chunk_size);

        // Subsequent get() returns the cached body.
        let (resp, _policy) = m.get("big").await.unwrap().unwrap();
        let cached = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(cached.len(), n_frames * chunk_size);
    }

    /// Tee path: dropping the response before draining aborts the
    /// background task; nothing gets committed to cacache.
    #[tokio::test]
    async fn tee_aborts_when_consumer_drops() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let response = streaming_response(20, 32 * 1024, 0xcc); // 640 KiB
        let policy = policy_for(&response);
        let url = "http://example.test/drop"
            .parse::<http_cache::Url>()
            .unwrap();
        let result = m
            .put("drop".to_string(), response, policy, url, None)
            .await
            .expect("tee put should succeed (handshake)");
        // Drop immediately — consumer never drains.
        drop(result);

        // Give the task time to notice the channel close + abort.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // No commit happened — entry should not be in cacache.
        let entry = cacache::metadata(dir.path(), "drop").await.unwrap();
        assert!(
            entry.is_none(),
            "consumer-drop should abort cache write, but entry persists: {:?}",
            entry
        );
    }

    /// Tee path: upstream body error mid-stream propagates to consumer
    /// and aborts the cache write.
    #[tokio::test]
    async fn tee_upstream_error_propagates_and_no_cache() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let frames: Vec<Result<Frame<Bytes>, std::io::Error>> = vec![
            Ok(Frame::data(Bytes::from(vec![0xee; 32 * 1024]))),
            Ok(Frame::data(Bytes::from(vec![0xee; 32 * 1024]))),
            Err(std::io::Error::other("upstream broke")),
        ];
        let body = http_body_util::StreamBody::new(futures_util::stream::iter(frames));
        let response = http::Response::builder().status(200).body(body).unwrap();
        let policy = policy_for(&response);
        let url = "http://example.test/err"
            .parse::<http_cache::Url>()
            .unwrap();
        let result = m
            .put("err".to_string(), response, policy, url, None)
            .await
            .expect("tee put returns immediately (background task surfaces the error)");

        // Drain. We expect to see partial bytes followed by an error.
        use http_body_util::BodyExt;
        let collected = result.into_body().collect().await;
        assert!(
            collected.is_err(),
            "consumer should see the upstream error, got Ok"
        );

        // Wait for the background task to clean up.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let entry = cacache::metadata(dir.path(), "err").await.unwrap();
        assert!(entry.is_none(), "errored stream should not be cached");
    }

    /// Regression for the put_tee commit/reinsert race window. Write
    /// an entry directly (bypassing put_tee) with `body_size: None`
    /// to simulate the in-flight state. get() must treat it as a miss
    /// — without this, get would set `remaining: entry.size = 0` and
    /// return a successful response with an empty body to concurrent
    /// readers during the (microsecond) window between writer.commit()
    /// and the size-fixing index reinsert.
    #[tokio::test]
    async fn get_treats_body_size_none_as_miss() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let disk = m.disk.as_ref().unwrap();

        // Hand-write an entry with body_size = None — the same state
        // the cacache index has between writer.commit() and the
        // post-commit reinsert in put_tee.
        let now = std::time::SystemTime::UNIX_EPOCH;
        let req = http::Request::get("http://example.test/").body(()).unwrap();
        let resp = http::Response::builder().status(200).body(()).unwrap();
        let policy = CachePolicy::new_options(
            &req.into_parts().0,
            &resp.into_parts().0,
            now,
            http_cache_semantics::CacheOptions::default(),
        );
        let meta = CacheMetadata {
            status: 200,
            version: 11,
            headers: vec![],
            policy,
            user_metadata: None,
            body_size: None, // in-flight sentinel
        };
        let blob = postcard::to_allocvec(&meta).unwrap();
        let body = b"actual body bytes here";
        let mut writer = cacache::WriteOpts::new()
            .raw_metadata(blob)
            // Mirror put_tee's initial commit: no .size() hint.
            .open(&disk.path, "in-flight")
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut writer, body)
            .await
            .unwrap();
        writer.commit().await.unwrap();

        // get() must return None — not a successful response with an
        // empty body — even though the entry is "present" in cacache.
        let hit = m.get("in-flight").await.unwrap();
        assert!(
            hit.is_none(),
            "get must return None for entries with body_size=None (race-window sentinel)"
        );
    }

    /// Tee path: a chunked response that turns out small enough still
    /// gets promoted to the mem tier (the per_entry_mem accumulator).
    #[tokio::test]
    async fn tee_promotes_small_chunked_to_mem() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = tmp_config(dir.path());
        // tmp_config sets max_memory_cache_bytes = Some(64 KiB), so
        // per_entry_mem ≈ 3.2 KiB. Bump it up so our 2-KiB-frame body
        // fits.
        cfg.max_memory_cache_bytes = Some(1024 * 1024); // 1 MiB → per_entry_mem = 50 KiB
        let m = PqcStreamingCacheManager::new(&cfg).unwrap();
        // 3 frames × 8 KiB = 24 KiB total, under per_entry_mem.
        let response = streaming_response(3, 8 * 1024, 0x77);
        let policy = policy_for(&response);
        let url = "http://example.test/small-chunked"
            .parse::<http_cache::Url>()
            .unwrap();
        let result = m
            .put("small-chunked".to_string(), response, policy, url, None)
            .await
            .unwrap();
        // Drain to drive the tee task to EOF.
        use http_body_util::BodyExt;
        let _ = result.into_body().collect().await.unwrap();

        // Wait for commit + mem insert.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if m.read_mem("small-chunked").await.is_some() {
                break;
            }
        }
        let from_mem = m.read_mem("small-chunked").await;
        assert!(
            from_mem.is_some(),
            "small chunked body should be promoted to mem tier after tee commit"
        );
        assert_eq!(from_mem.unwrap().len(), 3 * 8 * 1024);
    }
}

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
//! Two cacache entries per cached response:
//!   - `meta:<key>` — postcard-encoded `CacheMetadata` (status, headers,
//!     RFC policy, body size, optional user-metadata).
//!   - `body:<key>` — raw response bytes.
//!
//! On `get`, the metadata is read in full (it's small) and used to
//! construct the response head. The body is then streamed via
//! `cacache::Reader` (`AsyncRead`) in 64 KB chunks — large responses
//! never materialize in our process memory.
//!
//! On `put`, the body is buffered into `Bytes`, written to cacache via
//! `Writer::commit()` (atomic), then metadata is written. The write
//! ordering means a crash between the two leaves an orphan body that
//! gets reclaimed on the next `delete` / `clear` (cacache GCs blobs
//! when no key references them).
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

/// Postcard-serialized cache record (the "metadata" entry — body lives
/// in a separate cacache blob). On-disk format is private to this
/// module; need not match anything else.
#[derive(Serialize, Deserialize)]
struct CacheMetadata {
    status: u16,
    version: u8,
    /// Headers as a flat list to preserve multi-valued entries (e.g.
    /// `Set-Cookie`, `Vary`) without an outer HashMap collapsing them.
    headers: Vec<(String, Vec<u8>)>,
    body_size: u64,
    policy: CachePolicy,
    #[serde(default)]
    user_metadata: Option<Vec<u8>>,
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

fn meta_key(cache_key: &str) -> String {
    format!("meta:{cache_key}")
}

fn body_key(cache_key: &str) -> String {
    format!("body:{cache_key}")
}

/// Persistent byte-bounded disk tier. `bytes` is a running logical-size
/// counter so put/size stay O(1); `evict_lock` serializes both eviction
/// and `clear` so concurrent puts can't race a full rescan.
#[derive(Clone)]
struct DiskTier {
    path: PathBuf,
    max_bytes: u64,
    bytes: Arc<AtomicU64>,
    evict_lock: Arc<tokio::sync::Mutex<()>>,
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
            let path = PathBuf::from(d);
            let bytes = Arc::new(AtomicU64::new(0));
            // Seed off the constructor thread so a populated cache
            // doesn't block the UI on app launch.
            seed_byte_counter_async(path.clone(), Arc::clone(&bytes));
            DiskTier {
                path,
                max_bytes: config.max_cache_bytes.unwrap_or(DEFAULT_MAX_CACHE_BYTES),
                bytes,
                evict_lock: Arc::new(tokio::sync::Mutex::new(())),
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
            disk.bytes.store(0, Ordering::Release);
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

// Our concrete `Body` type. Two variants are enough because we always
// buffer body bytes into `Bytes` on put (mirroring upstream's
// `StreamingManager` design — see module doc); cached reads then
// stream via cacache::Reader.
//
// pin-project'd because the Cached variant pins an AsyncRead trait
// object.
pin_project! {
    #[project = PqcCachedBodyProj]
    pub enum PqcCachedBody {
        /// One-shot send of an already-resident `Bytes` blob — used for
        /// memory-tier hits, `convert_body` results, and `empty_body`.
        Buffered {
            data: Option<Bytes>,
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
            PqcCachedBodyProj::Buffered { data } => match data.take() {
                Some(b) if !b.is_empty() => Poll::Ready(Some(Ok(Frame::data(b)))),
                _ => Poll::Ready(None),
            },
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
    /// body — either Buffered (mem hit) or Cached (disk stream).
    fn response_head(meta: &CacheMetadata) -> http::response::Builder {
        let mut b = Response::builder()
            .status(StatusCode::from_u16(meta.status).unwrap_or(StatusCode::OK))
            .version(version_from_u8(meta.version));
        for (name, value) in &meta.headers {
            if let (Ok(n), Ok(v)) = (
                HeaderName::try_from(name.as_str()),
                HeaderValue::from_bytes(value),
            ) {
                b = b.header(n, v);
            }
        }
        b
    }
}

impl StreamingCacheManager for PqcStreamingCacheManager {
    type Body = PqcCachedBody;

    async fn get(
        &self,
        cache_key: &str,
    ) -> HttpCacheResult<Option<(Response<Self::Body>, CachePolicy)>> {
        let disk = match &self.disk {
            Some(d) => d,
            None => return Ok(None),
        };

        // Step 1: read the metadata blob (small; load fully). A miss
        // here is the most common case — be silent.
        let meta_bytes = match cacache::read(&disk.path, meta_key(cache_key)).await {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let meta: CacheMetadata = match postcard::from_bytes(&meta_bytes) {
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

        // Step 2: build the body. Memory tier first — if hit, the entire
        // body is already resident; no disk syscall needed.
        let body = if let Some(b) = self.read_mem(cache_key).await {
            PqcCachedBody::Buffered { data: Some(b) }
        } else {
            // Open a streaming reader. cacache::Reader is AsyncRead;
            // we stream in STREAM_CHUNK_SIZE chunks.
            match cacache::Reader::open(&disk.path, body_key(cache_key)).await {
                Ok(reader) => PqcCachedBody::Cached {
                    reader: Box::pin(reader),
                    buf: BytesMut::with_capacity(STREAM_CHUNK_SIZE),
                    done: false,
                    remaining: meta.body_size,
                },
                Err(_) => {
                    // Metadata existed but body didn't (crash between
                    // writes, or manual cacache clear) — treat as miss.
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
        let (parts, body) = response.into_parts();

        // Collect body to bytes. This matches upstream's StreamingManager
        // design — body is buffered during put then streamed during get.
        // The per_entry_disk cap (5% of total, ~1 MiB by default)
        // bounds memory pressure here.
        use http_body_util::BodyExt;
        let body_bytes = body
            .collect()
            .await
            .map_err(|e| StreamingError::new(e.into()))?
            .to_bytes();
        let body_size = body_bytes.len() as u64;

        // Cacheability gate: oversized → skip both tiers, return
        // Buffered (caller still gets the response, just uncached).
        let cacheable = match &self.disk {
            Some(d) => body_size <= self.per_entry_disk && body_size <= d.max_bytes,
            None => false,
        };

        if cacheable {
            if let Some(disk) = &self.disk {
                // Write body first. cacache's Writer atomically commits
                // (SHA-verified + atomic rename) and orphans the tmp
                // file on drop without commit.
                let mut writer = cacache::Writer::create(&disk.path, body_key(&cache_key))
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("body writer create: {e}")))?;
                tokio::io::AsyncWriteExt::write_all(&mut writer, &body_bytes)
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("body write: {e}")))?;
                writer
                    .commit()
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("body commit: {e}")))?;

                // Then write metadata.
                let headers: Vec<(String, Vec<u8>)> = parts
                    .headers
                    .iter()
                    .map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_owned()))
                    .collect();
                let meta = CacheMetadata {
                    status: parts.status.as_u16(),
                    version: version_to_u8(parts.version),
                    headers,
                    body_size,
                    policy,
                    user_metadata: metadata,
                };
                let meta_blob = postcard::to_allocvec(&meta)
                    .map_err(|e| HttpCacheError::cache(format!("meta serialize: {e}")))?;
                cacache::write(&disk.path, meta_key(&cache_key), &meta_blob)
                    .await
                    .map_err(|e| HttpCacheError::cache(format!("meta write: {e}")))?;

                // Bookkeeping: account both blobs against the byte counter.
                disk.bytes
                    .fetch_add(body_size + meta_blob.len() as u64, Ordering::AcqRel);

                // Best-effort eviction if over budget.
                self.evict_if_over_budget().await;
            }

            // Memory tier: only entries under the per-entry mem cap are
            // promoted. Avoids a single large response evicting many
            // small useful ones.
            if let Some(mem) = &self.mem {
                if body_size <= self.per_entry_mem {
                    mem.insert(cache_key.clone(), Arc::new(body_bytes.clone()))
                        .await;
                }
            }
        }

        // Return the response to the caller with our Body type.
        // It's Buffered regardless of cacheability — the caller drains
        // it the same way; cacheable vs not is invisible at this point.
        let mut b = Response::builder()
            .status(parts.status)
            .version(parts.version);
        for (name, value) in parts.headers.iter() {
            b = b.header(name, value);
        }
        let mut resp = b
            .body(PqcCachedBody::Buffered {
                data: Some(body_bytes),
            })
            .map_err(|e| HttpCacheError::cache(format!("build response: {e}")))?;
        *resp.extensions_mut() = parts.extensions;
        Ok(resp)
    }

    async fn convert_body<B>(&self, response: Response<B>) -> HttpCacheResult<Response<Self::Body>>
    where
        B: HttpBody + Send + 'static,
        B::Data: Send,
        B::Error: Into<StreamingError>,
    {
        let (parts, body) = response.into_parts();
        use http_body_util::BodyExt;
        let body_bytes = body
            .collect()
            .await
            .map_err(|e| StreamingError::new(e.into()))?
            .to_bytes();
        let mut b = Response::builder()
            .status(parts.status)
            .version(parts.version);
        for (name, value) in parts.headers.iter() {
            b = b.header(name, value);
        }
        let mut resp = b
            .body(PqcCachedBody::Buffered {
                data: Some(body_bytes),
            })
            .map_err(|e| HttpCacheError::cache(format!("build response: {e}")))?;
        *resp.extensions_mut() = parts.extensions;
        Ok(resp)
    }

    async fn delete(&self, cache_key: &str) -> HttpCacheResult<()> {
        if let Some(mem) = &self.mem {
            mem.invalidate(cache_key).await;
        }
        if let Some(disk) = &self.disk {
            // Account before remove so an intervening crash doesn't
            // double-decrement.
            let meta_sz = cacache::metadata(&disk.path, meta_key(cache_key))
                .await
                .ok()
                .flatten()
                .map(|m| m.size as u64)
                .unwrap_or(0);
            let body_sz = cacache::metadata(&disk.path, body_key(cache_key))
                .await
                .ok()
                .flatten()
                .map(|m| m.size as u64)
                .unwrap_or(0);
            let _ = cacache::remove(&disk.path, meta_key(cache_key)).await;
            let _ = cacache::remove(&disk.path, body_key(cache_key)).await;
            // cacache 13 doesn't expose explicit blob-GC. Orphan content
            // (no key references) lingers on disk until `clear()`. This
            // is acceptable for our use: the size-counter is logical
            // (sums key blob sizes), and the next `evict_if_over_budget`
            // pass will reclaim space if we drift over budget.
            disk.bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                    Some(v.saturating_sub(meta_sz + body_sz))
                })
                .ok();
        }
        Ok(())
    }

    fn empty_body(&self) -> Self::Body {
        PqcCachedBody::Buffered { data: None }
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

    /// Evict oldest entries until under budget. Best-effort; called
    /// after each put. Serialized via evict_lock so concurrent puts
    /// don't all race the same rescan.
    async fn evict_if_over_budget(&self) {
        let disk = match &self.disk {
            Some(d) => d,
            None => return,
        };
        if disk.bytes.load(Ordering::Acquire) <= disk.max_bytes {
            return;
        }
        let _guard = disk.evict_lock.lock().await;
        // Re-check inside the lock.
        if disk.bytes.load(Ordering::Acquire) <= disk.max_bytes {
            return;
        }
        // Walk index, evict oldest-first until under budget.
        let mut entries: Vec<(String, u64, std::time::SystemTime)> = Vec::new();
        for item in cacache::list_sync(&disk.path).flatten() {
            entries.push((
                item.key.clone(),
                item.size as u64,
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(item.time as u64),
            ));
        }
        entries.sort_by_key(|(_, _, t)| *t);
        let mut total = disk.bytes.load(Ordering::Acquire);
        for (key, size, _) in entries {
            if total <= disk.max_bytes {
                break;
            }
            if cacache::remove(&disk.path, &key).await.is_ok() {
                total = total.saturating_sub(size);
            }
        }
        disk.bytes.store(total, Ordering::Release);
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

/// Walk the cacache index off-thread to seed the byte counter. Run
/// once from `new`; the counter is otherwise updated incrementally.
fn seed_byte_counter_async(path: PathBuf, bytes: Arc<AtomicU64>) {
    tokio::spawn(async move {
        let mut total: u64 = 0;
        for item in cacache::list_sync(&path).flatten() {
            total = total.saturating_add(item.size as u64);
        }
        bytes.store(total, Ordering::Release);
    });
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
            max_inflight_total: Some(64),
            max_inflight_per_host: Some(5),
            enable_cache: true,
            cache_dir: Some(dir.to_string_lossy().into_owned()),
            max_cache_bytes: Some(1024 * 1024),      // 1 MiB
            max_memory_cache_bytes: Some(64 * 1024), // 64 KiB
        }
    }

    /// Direct-write a body + metadata pair so get/delete tests don't
    /// have to construct full Response<B> values. Bypasses the
    /// cacheability gate but exercises the exact storage layout
    /// `get` reads.
    async fn write_entry(m: &PqcStreamingCacheManager, key: &str, body: &[u8]) {
        let disk = m.disk.as_ref().expect("disk tier present");
        cacache::write(&disk.path, body_key(key), body)
            .await
            .unwrap();
        // Minimal CachePolicy + headers — postcard round-trip is all
        // we need for the test surface.
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
            body_size: body.len() as u64,
            policy,
            user_metadata: None,
        };
        let blob = postcard::to_allocvec(&meta).unwrap();
        cacache::write(&disk.path, meta_key(key), &blob)
            .await
            .unwrap();
        disk.bytes
            .fetch_add(body.len() as u64 + blob.len() as u64, Ordering::AcqRel);
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

    #[tokio::test]
    async fn get_returns_none_on_corrupt_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let m = PqcStreamingCacheManager::new(&tmp_config(dir.path())).unwrap();
        let disk = m.disk.as_ref().unwrap();
        // Write garbage where metadata should be — postcard will fail
        // to deserialize and we treat that as a cache miss (matches
        // upstream's #141 fix).
        cacache::write(&disk.path, meta_key("k1"), &[0xff, 0xff, 0xff, 0xff])
            .await
            .unwrap();
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
}

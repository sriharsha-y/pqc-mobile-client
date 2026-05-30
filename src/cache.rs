//! Opt-in RFC 9111 HTTP response cache (the `cache` cargo feature).
//!
//! RFC semantics (freshness, revalidation, `Vary`, auth rules) come from the
//! `http-cache` / `http-cache-semantics` stack, so cacheability is decided by
//! method + status + headers, never by file type — like OkHttp `Cache` /
//! `URLCache`. See docs/{android,ios}.md for the consumer-facing narrative.
//!
//! This module supplies the storage the bundled managers lack: [`PqcCacheManager`]
//! is a private (`shared = false`), byte-bounded `cacache` disk tier, fronted by
//! a `moka` in-memory tier on iOS (mirroring `URLCache`'s mem+disk; Android is
//! disk-only like OkHttp).
//!
//! One intentional divergence from a strict native LRU: disk eviction is by
//! insertion time, not access time (cacache exposes no access time; the iOS
//! memory tier is true LRU).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use http_cache::{CacheManager, HttpResponse, Result as HttpCacheResult};
use http_cache_semantics::CachePolicy;
use serde::{Deserialize, Serialize};

use crate::config::PqcConfig;

/// On-disk cap when `max_cache_bytes` is `None`: 20 MiB, a typical `URLCache`
/// disk capacity.
const DEFAULT_MAX_CACHE_BYTES: u64 = 20 * 1024 * 1024;

/// In-memory hot-tier cap (iOS only): 4 MiB, matching `URLCache`'s historical
/// memory capacity.
#[cfg(target_os = "ios")]
const DEFAULT_MEM_CACHE_BYTES: u64 = 4 * 1024 * 1024;

/// The persisted cache record: the response plus the RFC policy needed to
/// revalidate it. Serialized with postcard (compact serde binary). The on-disk
/// format is private to this manager, so it need not match anything else.
#[derive(Serialize, Deserialize)]
struct Store {
    response: HttpResponse,
    policy: CachePolicy,
}

/// Persistent byte-bounded disk tier. `bytes` is a running logical-size
/// counter (OkHttp `DiskLruCache`-style) so put/size stay O(1); `evict_lock`
/// serializes both eviction and `clear` so concurrent puts can't race a full
/// rescan. All `Arc` so clones share one ledger.
#[derive(Clone)]
struct DiskTier {
    path: PathBuf,
    max_bytes: u64,
    bytes: Arc<AtomicU64>,
    evict_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
pub struct PqcCacheManager {
    disk: Option<DiskTier>,
    /// Used only on iOS; `None` elsewhere (Android = disk-only like OkHttp).
    mem: Option<moka::future::Cache<String, Arc<Vec<u8>>>>,
    /// Per-entry size cap. Equals `disk.max_bytes` when disk is present;
    /// otherwise the mem tier's capacity. Guards both tiers symmetrically so a
    /// single oversized response can't evict the entire mem-only cache.
    entry_max_bytes: u64,
}

impl PqcCacheManager {
    pub fn new(config: &PqcConfig) -> Option<Self> {
        let disk = config.cache_dir.as_ref().map(|d| {
            let path = PathBuf::from(d);
            let bytes = Arc::new(AtomicU64::new(0));
            // Seed the counter off the constructor thread so a populated cache
            // (after weeks of use) doesn't block the UI thread on app launch.
            // Counter starts at 0; eviction self-heals on the first over-budget
            // put if the seed thread hasn't finished by then.
            seed_byte_counter_async(path.clone(), Arc::clone(&bytes));
            DiskTier {
                path,
                max_bytes: config.max_cache_bytes.unwrap_or(DEFAULT_MAX_CACHE_BYTES),
                bytes,
                evict_lock: Arc::new(tokio::sync::Mutex::new(())),
            }
        });
        let mem = build_mem_tier();

        if disk.is_none() && mem.is_none() {
            return None;
        }
        let entry_max_bytes = match (&disk, &mem) {
            (Some(d), _) => d.max_bytes,
            (None, Some(_)) => mem_tier_capacity(),
            (None, None) => unreachable!(),
        };
        Some(Self {
            disk,
            mem,
            entry_max_bytes,
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
            // Serialize against eviction and re-seed the counter from disk
            // after the wipe — `cacache::clear` then re-scan inside one
            // critical section. A put racing with clear can still write to
            // disk between the two cacache calls, but its record is then
            // counted by the re-scan, so the counter never undercounts what's
            // actually on disk.
            let _guard = disk.evict_lock.lock().await;
            if let Err(e) = cacache::clear(&disk.path).await {
                log::warn!("pqc cache: clear failed: {e}");
            }
            let path = disk.path.clone();
            let remaining = tokio::task::spawn_blocking(move || -> u64 {
                cacache::list_sync(&path)
                    .filter_map(|r| r.ok())
                    .map(|m| m.size as u64)
                    .sum()
            })
            .await
            .unwrap_or(0);
            disk.bytes.store(remaining, Ordering::Relaxed);
        }
    }

    /// Total bytes indexed in the on-disk tier (the persistent figure; mem is
    /// a hot subset). `0` when there is no disk tier. O(1).
    pub async fn size(&self) -> u64 {
        self.disk
            .as_ref()
            .map_or(0, |disk| disk.bytes.load(Ordering::Relaxed))
    }

    /// Evict oldest-first until the disk tier is back under its byte budget.
    /// Serialized by `evict_lock` so concurrent puts don't each rescan; the
    /// pass recomputes the true total from disk, so incremental-accounting
    /// drift self-heals here.
    async fn evict_disk_if_needed(&self, disk: &DiskTier) {
        if disk.bytes.load(Ordering::Relaxed) <= disk.max_bytes {
            return;
        }
        let _guard = disk.evict_lock.lock().await;
        if disk.bytes.load(Ordering::Relaxed) <= disk.max_bytes {
            return;
        }

        let path = disk.path.clone();
        let max_bytes = disk.max_bytes;
        let remaining = tokio::task::spawn_blocking(move || -> u64 {
            let mut entries: Vec<cacache::Metadata> =
                cacache::list_sync(&path).filter_map(|r| r.ok()).collect();
            let mut total: u64 = entries.iter().map(|m| m.size as u64).sum();
            if total <= max_bytes {
                return total;
            }
            // Oldest insertion time first (cacache has no access time → FIFO,
            // a documented approximation of the native LRU).
            entries.sort_by_key(|m| m.time);
            for m in entries {
                if total <= max_bytes {
                    break;
                }
                if cacache::remove_sync(&path, &m.key).is_ok() {
                    // Drop the content blob too; remove_sync only drops the
                    // index entry. cacache content-dedups by hash, so the rare
                    // shared-blob case downgrades a co-referencing entry to a
                    // refetch — not corruption.
                    let _ = cacache::remove_hash_sync(&path, &m.integrity);
                    total = total.saturating_sub(m.size as u64);
                }
            }
            total
        })
        .await
        .unwrap_or_else(|_| disk.bytes.load(Ordering::Relaxed));

        disk.bytes.store(remaining, Ordering::Relaxed);
    }
}

/// Seed the running byte counter in a background thread so PqcHttpClient::new
/// returns immediately — `cacache::list_sync` is blocking and a populated
/// cache (after weeks of use) can stall it for hundreds of ms.
fn seed_byte_counter_async(path: PathBuf, bytes: Arc<AtomicU64>) {
    std::thread::Builder::new()
        .name("pqc-cache-seed".into())
        .spawn(move || {
            let total: u64 = cacache::list_sync(&path)
                .filter_map(|r| r.ok())
                .map(|m| m.size as u64)
                .sum();
            // fetch_add (not store) so any puts that landed while we were
            // scanning are not erased — we add what we found on disk on top of
            // whatever the live put already accounted for.
            bytes.fetch_add(total, Ordering::Relaxed);
        })
        .ok();
}

fn build_mem_tier() -> Option<moka::future::Cache<String, Arc<Vec<u8>>>> {
    #[cfg(target_os = "ios")]
    {
        Some(
            moka::future::Cache::builder()
                .max_capacity(DEFAULT_MEM_CACHE_BYTES)
                .weigher(|_k: &String, v: &Arc<Vec<u8>>| v.len().try_into().unwrap_or(u32::MAX))
                .build(),
        )
    }
    // Non-iOS (Android, host): disk-only, like OkHttp's `Cache`.
    #[cfg(not(target_os = "ios"))]
    {
        None
    }
}

fn mem_tier_capacity() -> u64 {
    #[cfg(target_os = "ios")]
    {
        DEFAULT_MEM_CACHE_BYTES
    }
    #[cfg(not(target_os = "ios"))]
    {
        0
    }
}

#[async_trait::async_trait]
impl CacheManager for PqcCacheManager {
    async fn get(&self, cache_key: &str) -> HttpCacheResult<Option<(HttpResponse, CachePolicy)>> {
        // Memory tier first (iOS).
        if let Some(mem) = &self.mem {
            if let Some(bytes) = mem.get(cache_key).await {
                if let Ok(store) = postcard::from_bytes::<Store>(&bytes) {
                    return Ok(Some((store.response, store.policy)));
                }
            }
        }
        // Disk tier. A read/deserialize failure is treated as a miss, never an
        // error, so a corrupt or absent entry can't break the request.
        if let Some(disk) = &self.disk {
            if let Ok(bytes) = cacache::read(&disk.path, cache_key).await {
                let store: Store = match postcard::from_bytes(&bytes) {
                    Ok(s) => s,
                    Err(_) => return Ok(None),
                };
                if let Some(mem) = &self.mem {
                    mem.insert(cache_key.to_string(), Arc::new(bytes)).await;
                }
                return Ok(Some((store.response, store.policy)));
            }
        }
        Ok(None)
    }

    async fn put(
        &self,
        cache_key: String,
        response: HttpResponse,
        policy: CachePolicy,
    ) -> HttpCacheResult<HttpResponse> {
        let store = Store {
            response: response.clone(),
            policy,
        };
        // Serialization failure must not fail the request — just don't cache.
        let bytes = match postcard::to_allocvec(&store) {
            Ok(b) => b,
            Err(_) => return Ok(response),
        };
        let new_len = bytes.len() as u64;

        // Refuse oversized entries from BOTH tiers: on disk-only it would
        // thrash eviction; on mem-only (iOS, no cache_dir) admitting one
        // near-cap entry would evict the entire hot set, contradicting the
        // "won't exceed max_cache_bytes" contract.
        if new_len > self.entry_max_bytes {
            return Ok(response);
        }

        if let Some(disk) = &self.disk {
            // cacache keeps the previous content blob on overwrite (e.g.
            // revalidation); reclaim it unless byte-identical (same integrity
            // → same shared blob).
            let prev = cacache::metadata(&disk.path, &cache_key)
                .await
                .ok()
                .flatten();
            if let Ok(new_sri) = cacache::write(&disk.path, &cache_key, &bytes).await {
                disk.bytes.fetch_add(new_len, Ordering::Relaxed);
                if let Some(prev) = prev {
                    let _ = disk
                        .bytes
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                            Some(v.saturating_sub(prev.size as u64))
                        });
                    if prev.integrity != new_sri {
                        let _ = cacache::remove_hash(&disk.path, &prev.integrity).await;
                    }
                }
                self.evict_disk_if_needed(disk).await;
            }
        }
        if let Some(mem) = &self.mem {
            mem.insert(cache_key, Arc::new(bytes)).await;
        }
        Ok(response)
    }

    async fn delete(&self, cache_key: &str) -> HttpCacheResult<()> {
        if let Some(mem) = &self.mem {
            mem.invalidate(cache_key).await;
        }
        if let Some(disk) = &self.disk {
            // Reclaim the content blob too — http-cache calls delete() on
            // every non-cacheable request with a matching cached GET (RFC
            // invalidation), so orphan blobs would creep past max_cache_bytes.
            let meta = cacache::metadata(&disk.path, cache_key)
                .await
                .ok()
                .flatten();
            let _ = cacache::remove(&disk.path, cache_key).await;
            if let Some(m) = meta {
                let _ = disk
                    .bytes
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                        Some(v.saturating_sub(m.size as u64))
                    });
                let _ = cacache::remove_hash(&disk.path, &m.integrity).await;
            }
        }
        Ok(())
    }
}

/// Build the reqwest-middleware client that fronts `base` with the cache.
/// A *private* cache (`shared = false`): honors `no-store`/`no-cache`, and —
/// like the native private caches — caches authenticated responses when their
/// headers permit. TLS / PQC / pinning are untouched (the middleware wraps the
/// already-built client).
pub fn build_cached_client(
    base: reqwest::Client,
    manager: PqcCacheManager,
) -> reqwest_middleware::ClientWithMiddleware {
    use http_cache_reqwest::{Cache, CacheMode, CacheOptions, HttpCache, HttpCacheOptions};

    reqwest_middleware::ClientBuilder::new(base)
        .with(Cache(HttpCache {
            mode: CacheMode::Default,
            manager,
            options: HttpCacheOptions {
                cache_options: Some(CacheOptions {
                    shared: false,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }))
        .build()
}

#[cfg(test)]
mod tests {
    //! Cover the storage layer we own (the disk tier — `mem` is `None` off
    //! iOS, so host tests exercise the Android-equivalent path). RFC 9111
    //! semantics are the upstream `http-cache-semantics` crate's concern.
    use super::*;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique, freshly-empty temp dir for one test.
    fn tmp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("pqc-cache-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn disk_mgr(dir: &Path, max_bytes: u64) -> PqcCacheManager {
        // Fresh temp dirs → counter starts at 0; the real `new()` would seed
        // it asynchronously from disk.
        PqcCacheManager {
            disk: Some(DiskTier {
                path: dir.to_path_buf(),
                max_bytes,
                bytes: Arc::new(AtomicU64::new(0)),
                evict_lock: Arc::new(tokio::sync::Mutex::new(())),
            }),
            mem: None,
            entry_max_bytes: max_bytes,
        }
    }

    /// A storable policy (`max-age=60`, status 200) for a GET.
    fn fresh_policy() -> CachePolicy {
        let req = http::Request::get("https://example.com/x")
            .body(())
            .unwrap();
        let res = http::Response::builder()
            .status(200)
            .header("cache-control", "max-age=60")
            .body(())
            .unwrap();
        CachePolicy::new(&req, &res)
    }

    fn resp(body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            body,
            headers: HashMap::new(),
            status: 200,
            url: "https://example.com/x".parse().unwrap(),
            version: http_cache::HttpVersion::Http11,
        }
    }

    #[tokio::test]
    async fn put_then_get_roundtrips() {
        let dir = tmp_dir();
        let m = disk_mgr(&dir, 1 << 20);
        m.put("k1".into(), resp(b"hello".to_vec()), fresh_policy())
            .await
            .unwrap();
        let got = m.get("k1").await.unwrap();
        assert!(got.is_some(), "stored entry should be retrievable");
        assert_eq!(got.unwrap().0.body, b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn size_grows_then_clear_zeroes() {
        let dir = tmp_dir();
        let m = disk_mgr(&dir, 1 << 20);
        assert_eq!(m.size().await, 0);
        m.put("k1".into(), resp(vec![0u8; 4096]), fresh_policy())
            .await
            .unwrap();
        assert!(m.size().await > 0, "size should grow after a store");
        m.clear().await;
        assert_eq!(m.size().await, 0, "clear() must empty the cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn oversized_entry_is_not_stored() {
        let dir = tmp_dir();
        // Budget smaller than the body → entry must be skipped, not stored.
        let m = disk_mgr(&dir, 64);
        m.put("big".into(), resp(vec![0u8; 8192]), fresh_policy())
            .await
            .unwrap();
        assert!(
            m.get("big").await.unwrap().is_none(),
            "oversized entry must not be stored"
        );
        assert_eq!(m.size().await, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_removes_entry_and_reclaims_content() {
        let dir = tmp_dir();
        let m = disk_mgr(&dir, 1 << 20);
        m.put("k1".into(), resp(vec![7u8; 4096]), fresh_policy())
            .await
            .unwrap();
        assert!(m.size().await > 0);
        m.delete("k1").await.unwrap();
        assert!(m.get("k1").await.unwrap().is_none());
        // delete() must reclaim the content blob, not just the index entry —
        // otherwise the blob orphans and disk creeps past max_cache_bytes.
        assert_eq!(
            m.size().await,
            0,
            "delete() should reclaim the content blob"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn eviction_keeps_under_budget_and_drops_oldest() {
        let dir = tmp_dir();
        // ~5 KiB bodies, 12 KiB budget → only the newest ~2 entries survive.
        let body = 5 * 1024;
        let budget = 12 * 1024;
        let m = disk_mgr(&dir, budget);
        for i in 0..4 {
            m.put(format!("k{i}"), resp(vec![i as u8; body]), fresh_policy())
                .await
                .unwrap();
            // cacache timestamps are ms-resolution; space writes so the
            // oldest-first eviction order is deterministic.
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        assert!(
            m.size().await <= budget,
            "cache must stay within its byte budget"
        );
        assert!(
            m.get("k0").await.unwrap().is_none(),
            "oldest entry should be evicted"
        );
        assert!(
            m.get("k3").await.unwrap().is_some(),
            "newest entry should survive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrite_reclaims_old_blob() {
        let dir = tmp_dir();
        let m = disk_mgr(&dir, 1 << 20);
        m.put("k1".into(), resp(vec![1u8; 4096]), fresh_policy())
            .await
            .unwrap();
        let after_first = m.size().await;
        // Overwrite the same key with a same-sized but different body. Without
        // blob reclamation the old content would linger and size would grow.
        m.put("k1".into(), resp(vec![2u8; 4096]), fresh_policy())
            .await
            .unwrap();
        let after_overwrite = m.size().await;
        assert!(
            after_overwrite <= after_first + 256,
            "overwrite must reclaim the old blob (first={after_first}, after={after_overwrite})"
        );
        // And the latest body wins.
        assert_eq!(m.get("k1").await.unwrap().unwrap().0.body, vec![2u8; 4096]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

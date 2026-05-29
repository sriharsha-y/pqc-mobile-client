//! Opt-in RFC 9111 HTTP response cache (the `cache` cargo feature).
//!
//! The RFC semantics — freshness, conditional revalidation, `Vary`, age,
//! heuristics, authenticated-response rules — are owned by the proven
//! `http-cache` / `http-cache-semantics` stack (the same engine OkHttp- and
//! browser-class caches rely on). Cacheability is therefore decided by request
//! method + response status + cache headers, **never** by file type, exactly
//! like the platform caches (Android OkHttp `Cache`, iOS `URLCache`).
//!
//! What this module adds is the storage backend the bundled managers don't
//! give us: [`PqcCacheManager`] is a **persistent, byte-bounded** disk tier
//! (cacache) — like OkHttp's `Cache(dir, maxSize)` and `URLCache`'s disk store
//! — optionally fronted by an **in-memory** tier (moka) on iOS to mirror
//! `URLCache`'s memory+disk composite. It is configured as a *private* cache
//! (`shared = false`) by the client.
//!
//! Two deliberate, documented divergences from the native LRU:
//!   * disk eviction is by **insertion time** (cacache exposes no access
//!     time), an approximation of LRU; the iOS memory tier is true LRU.
//!   * `max_body_bytes` (the decompression-bomb cap) is **not** applied to the
//!     middleware's internal fetch of a cacheable GET/HEAD — matching OkHttp,
//!     which has no such cap (iOS's URLSession guard never applied to our
//!     reqwest path). The client still caps the body it hands back, and this
//!     manager refuses to *store* entries larger than the disk budget.

use std::path::PathBuf;
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

/// The persisted cache record. Same `{ response, policy }` shape the bundled
/// http-cache managers use, so the on-disk encoding stays conventional.
#[derive(Serialize, Deserialize)]
struct Store {
    response: HttpResponse,
    policy: CachePolicy,
}

/// Persistent byte-bounded disk tier.
#[derive(Clone)]
struct DiskTier {
    path: PathBuf,
    max_bytes: u64,
}

/// Our [`CacheManager`]: a byte-bounded cacache disk tier, optionally fronted
/// by a moka in-memory tier (iOS). Cheap to clone (the path is a handle, moka
/// is `Arc`-backed); clones share the same underlying stores, so the copy held
/// by the middleware and the copy the client keeps for `clear`/`size` operate
/// on one cache.
#[derive(Clone)]
pub struct PqcCacheManager {
    disk: Option<DiskTier>,
    /// Present (and used) only on iOS. `None` elsewhere, giving Android the
    /// disk-only behavior of OkHttp's `Cache`.
    mem: Option<moka::future::Cache<String, Arc<Vec<u8>>>>,
}

impl PqcCacheManager {
    /// Build a manager from config, or `None` if no tier is available (e.g.
    /// Android with no `cache_dir` — there is nowhere to cache).
    pub fn new(config: &PqcConfig) -> Option<Self> {
        let disk = config.cache_dir.as_ref().map(|d| DiskTier {
            path: PathBuf::from(d),
            max_bytes: config.max_cache_bytes.unwrap_or(DEFAULT_MAX_CACHE_BYTES),
        });
        let mem = build_mem_tier();

        if disk.is_none() && mem.is_none() {
            return None;
        }
        Some(Self { disk, mem })
    }

    /// Clear all cached responses (best-effort; mirrors the non-throwing
    /// `URLCache.removeAllCachedResponses` / OkHttp `Cache.evictAll`).
    pub async fn clear(&self) {
        if let Some(mem) = &self.mem {
            mem.invalidate_all();
            mem.run_pending_tasks().await;
        }
        if let Some(disk) = &self.disk {
            if let Err(e) = cacache::clear(&disk.path).await {
                log::warn!("pqc cache: clear failed: {e}");
            }
        }
    }

    /// Total bytes currently indexed in the on-disk tier (the persistent,
    /// native-meaningful figure; the memory tier is a hot subset). `0` when
    /// there is no disk tier.
    pub async fn size(&self) -> u64 {
        let Some(disk) = &self.disk else { return 0 };
        let path = disk.path.clone();
        // cacache's listing is sync/blocking; keep it off the async worker.
        tokio::task::spawn_blocking(move || {
            cacache::list_sync(&path)
                .filter_map(|r| r.ok())
                .map(|m| m.size as u64)
                .sum()
        })
        .await
        .unwrap_or(0)
    }

    /// Evict oldest-first until the disk tier is back under its byte budget.
    /// Reclaims both the index entry and its content blob.
    async fn evict_disk_if_needed(&self, disk: &DiskTier) {
        let path = disk.path.clone();
        let max_bytes = disk.max_bytes;
        let _ = tokio::task::spawn_blocking(move || {
            let mut entries: Vec<cacache::Metadata> =
                cacache::list_sync(&path).filter_map(|r| r.ok()).collect();
            let mut total: u64 = entries.iter().map(|m| m.size as u64).sum();
            if total <= max_bytes {
                return;
            }
            // Oldest insertion time first (cacache has no access time, so this
            // is FIFO — a documented approximation of the native LRU).
            entries.sort_by_key(|m| m.time);
            for m in entries {
                if total <= max_bytes {
                    break;
                }
                if cacache::remove_sync(&path, &m.key).is_ok() {
                    // Reclaim the content blob too; remove_sync only drops the
                    // index entry. Bodies are unique per URL, so the blob is
                    // not shared in practice.
                    let _ = cacache::remove_hash_sync(&path, &m.integrity);
                    total = total.saturating_sub(m.size as u64);
                }
            }
        })
        .await;
    }
}

/// iOS: a byte-bounded moka tier mirroring `URLCache`'s memory tier.
#[cfg(target_os = "ios")]
fn build_mem_tier() -> Option<moka::future::Cache<String, Arc<Vec<u8>>>> {
    Some(
        moka::future::Cache::builder()
            .max_capacity(DEFAULT_MEM_CACHE_BYTES)
            .weigher(|_k: &String, v: &Arc<Vec<u8>>| v.len().try_into().unwrap_or(u32::MAX))
            .build(),
    )
}

/// Non-iOS (Android, host): disk-only, like OkHttp's `Cache`.
#[cfg(not(target_os = "ios"))]
fn build_mem_tier() -> Option<moka::future::Cache<String, Arc<Vec<u8>>>> {
    None
}

#[async_trait::async_trait]
impl CacheManager for PqcCacheManager {
    async fn get(&self, cache_key: &str) -> HttpCacheResult<Option<(HttpResponse, CachePolicy)>> {
        // Memory tier first (iOS).
        if let Some(mem) = &self.mem {
            if let Some(bytes) = mem.get(cache_key).await {
                if let Ok(store) = bincode::deserialize::<Store>(&bytes) {
                    return Ok(Some((store.response, store.policy)));
                }
            }
        }
        // Disk tier. A read/deserialize failure is treated as a miss, never an
        // error, so a corrupt or absent entry can't break the request.
        if let Some(disk) = &self.disk {
            if let Ok(bytes) = cacache::read(&disk.path, cache_key).await {
                let store: Store = match bincode::deserialize(&bytes) {
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
        // A serialization failure must not fail the request — just don't cache.
        let bytes = match bincode::serialize(&store) {
            Ok(b) => b,
            Err(_) => return Ok(response),
        };

        if let Some(disk) = &self.disk {
            // Never persist an entry larger than the whole budget; it would
            // only thrash eviction. (The client still caps what it returns.)
            if (bytes.len() as u64) <= disk.max_bytes {
                // cacache keeps the previous content blob when a key is
                // overwritten (e.g. on revalidation), so reclaim it to avoid
                // unbounded orphan growth — unless the new content is
                // byte-identical (same integrity → same shared blob).
                let prev = cacache::metadata(&disk.path, &cache_key)
                    .await
                    .ok()
                    .flatten();
                if let Ok(new_sri) = cacache::write(&disk.path, &cache_key, &bytes).await {
                    if let Some(prev) = prev {
                        if prev.integrity != new_sri {
                            let _ = cacache::remove_hash(&disk.path, &prev.integrity).await;
                        }
                    }
                    self.evict_disk_if_needed(disk).await;
                }
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
            let _ = cacache::remove(&disk.path, cache_key).await;
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

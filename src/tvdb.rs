use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use std::time::Instant;

use chrono::Datelike as _;
use chrono::NaiveDate;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::TvdbConfig;
use crate::report::AnyResult;

const BASE_URL: &str = "https://api4.thetvdb.com";
/// Refresh the token after 25 days; TVDB tokens expire at 30.
const TOKEN_TTL: Duration = Duration::from_secs(25 * 24 * 60 * 60);

// ── public output types ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ShowMeta {
    pub canonical_title: String,
    pub release_year: Option<u32>,
    pub imdb_id: Option<String>,
    pub season: u32,
    pub episode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieMeta {
    pub canonical_title: String,
    pub release_year: Option<u32>,
    pub imdb_id: Option<String>,
}

// ── client ───────────────────────────────────────────────────────────────────

/// Cache key: `(lowercase_title, year_hint)`.
type CacheKey = (String, Option<u32>);

/// TVDB-derived show data, independent of the per-call season/episode numbers.
/// Cached so repeated lookups for the same series (e.g. a season pack) hit the
/// network only once.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeriesBase {
    canonical_title: String,
    release_year: Option<u32>,
    imdb_id: Option<String>,
}

impl SeriesBase {
    fn to_show_meta(&self, season: u32, episode: u32) -> ShowMeta {
        ShowMeta {
            canonical_title: self.canonical_title.clone(),
            release_year: self.release_year,
            imdb_id: self.imdb_id.clone(),
            season,
            episode,
        }
    }
}

#[derive(Debug)]
pub struct Client {
    api_key: String,
    http: reqwest::Client,
    token: RwLock<Option<CachedToken>>,
    show_cache: RwLock<HashMap<CacheKey, SeriesBase>>,
    movie_cache: RwLock<HashMap<CacheKey, MovieMeta>>,
    retry_attempts: u32,
    retry_delay: Duration,
    /// Persistent backing store for `show_cache`; `None` when no cache path is configured.
    show_tree: Option<sled::Tree>,
    /// Persistent backing store for `movie_cache`; `None` when no cache path is configured.
    movie_tree: Option<sled::Tree>,
}

#[derive(Debug)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl Client {
    pub fn new(config: &TvdbConfig) -> AnyResult<Self> {
        let (show_cache, movie_cache, show_tree, movie_tree) = match &config.cache_path {
            None => (HashMap::new(), HashMap::new(), None, None),
            Some(path) => {
                let db = sled::open(path.as_std_path())
                    .map_err(|e| eyre::eyre!("failed to open TVDB cache at {path}: {e}"))?;
                let show_tree = db
                    .open_tree("shows")
                    .map_err(|e| eyre::eyre!("failed to open shows tree: {e}"))?;
                let movie_tree = db
                    .open_tree("movies")
                    .map_err(|e| eyre::eyre!("failed to open movies tree: {e}"))?;

                let show_cache = load_tree(&show_tree, "show");
                let movie_cache = load_tree(&movie_tree, "movie");

                (show_cache, movie_cache, Some(show_tree), Some(movie_tree))
            }
        };

        Ok(Self {
            api_key: config.tvdb_api_key.clone(),
            http: reqwest::Client::new(),
            token: RwLock::new(None),
            show_cache: RwLock::new(show_cache),
            movie_cache: RwLock::new(movie_cache),
            retry_attempts: config.retry_attempts.max(1),
            retry_delay: config.retry_delay.into(),
            show_tree,
            movie_tree,
        })
    }

    #[tracing::instrument(skip(self), fields(title = %title, year_hint = ?year_hint))]
    pub async fn enrich_show(
        &self,
        title: &str,
        year_hint: Option<u32>,
        season: u32,
        episode: u32,
    ) -> AnyResult<Option<ShowMeta>> {
        let key: CacheKey = (title.to_lowercase(), year_hint);

        // Cache hit — no network call needed.
        {
            let guard = self.show_cache.read().await;
            if let Some(base) = guard.get(&key) {
                tracing::debug!(title = %title, "show cache hit");
                return Ok(Some(base.to_show_meta(season, episode)));
            }
        }

        // Cache miss — query TVDB, then populate the cache.
        let token = self.token().await?;
        let results = retry(self.retry_attempts, self.retry_delay, "search", || {
            self.search(&token, title, "series")
        })
        .await?;

        let Some(result) = pick_result(&results, year_hint) else {
            tracing::debug!(title = %title, "no TVDB series match");
            return Ok(None);
        };
        let tvdb_id = parse_tvdb_id(result.tvdb_id.as_ref())?;
        let extended = retry(
            self.retry_attempts,
            self.retry_delay,
            "series_extended",
            || self.series_extended(&token, tvdb_id),
        )
        .await?;

        let base = SeriesBase {
            canonical_title: extended.name,
            release_year: extended
                .first_aired
                .and_then(|d| u32::try_from(d.year()).ok()),
            imdb_id: extract_imdb(extended.remote_ids.as_deref()),
        };
        let meta = base.to_show_meta(season, episode);
        if let Some(tree) = &self.show_tree {
            persist_entry(tree, &key, &base, "show");
        }
        self.show_cache.write().await.insert(key, base);
        Ok(Some(meta))
    }

    #[tracing::instrument(skip(self), fields(title = %title, year_hint = ?year_hint))]
    pub async fn enrich_movie(
        &self,
        title: &str,
        year_hint: Option<u32>,
    ) -> AnyResult<Option<MovieMeta>> {
        let key: CacheKey = (title.to_lowercase(), year_hint);

        // Cache hit — no network call needed.
        {
            let guard = self.movie_cache.read().await;
            if let Some(entry) = guard.get(&key) {
                tracing::debug!(title = %title, "movie cache hit");
                return Ok(Some(entry.clone()));
            }
        }

        // Cache miss — query TVDB, then populate the cache.
        let token = self.token().await?;
        let results = retry(self.retry_attempts, self.retry_delay, "search", || {
            self.search(&token, title, "movie")
        })
        .await?;

        let Some(result) = pick_result(&results, year_hint) else {
            tracing::debug!(title = %title, "no TVDB movie match");
            return Ok(None);
        };
        let tvdb_id = parse_tvdb_id(result.tvdb_id.as_ref())?;
        let extended = retry(
            self.retry_attempts,
            self.retry_delay,
            "movie_extended",
            || self.movie_extended(&token, tvdb_id),
        )
        .await?;

        let release_year = extended
            .year
            .as_deref()
            .and_then(|y| y.parse().ok())
            .or_else(|| {
                extended
                    .releases
                    .as_deref()?
                    .iter()
                    .find_map(|r| r.date.and_then(|d| u32::try_from(d.year()).ok()))
            });

        let meta = MovieMeta {
            canonical_title: extended.name,
            release_year,
            imdb_id: extract_imdb(extended.remote_ids.as_deref()),
        };
        if let Some(tree) = &self.movie_tree {
            persist_entry(tree, &key, &meta, "movie");
        }
        self.movie_cache.write().await.insert(key, meta.clone());
        Ok(Some(meta))
    }

    // ── lifecycle ────────────────────────────────────────────────────────────

    /// Flush pending sled writes to disk.  A no-op when no cache path is
    /// configured.  Errors are logged and swallowed — callers should invoke
    /// this on clean shutdown but need not treat failure as fatal.
    pub async fn flush(&self) {
        for (tree, label) in [
            (self.show_tree.as_ref(), "show"),
            (self.movie_tree.as_ref(), "movie"),
        ] {
            if let Some(tree) = tree
                && let Err(e) = tree.flush_async().await
            {
                tracing::warn!(label, error = ?e, "failed to flush TVDB cache tree");
            }
        }
    }

    // ── token management ─────────────────────────────────────────────────────

    async fn token(&self) -> AnyResult<String> {
        // Fast path: valid cached token
        {
            let guard = self.token.read().await;
            if let Some(cached) = &*guard
                && cached.expires_at > Instant::now()
            {
                return Ok(cached.value.clone());
            }
        }

        // Slow path: refresh under write lock
        let mut guard = self.token.write().await;
        // Double-check: another task may have refreshed while we waited
        if let Some(cached) = &*guard
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.value.clone());
        }

        tracing::debug!("refreshing TVDB token");
        let value = retry(self.retry_attempts, self.retry_delay, "login", || {
            self.login()
        })
        .await?;
        *guard = Some(CachedToken {
            value: value.clone(),
            expires_at: Instant::now() + TOKEN_TTL,
        });
        Ok(value)
    }

    // ── raw API calls ─────────────────────────────────────────────────────────

    #[tracing::instrument(skip_all)]
    async fn login(&self) -> AnyResult<String> {
        #[derive(Serialize)]
        struct Body<'a> {
            apikey: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            data: RespData,
        }
        #[derive(Deserialize)]
        struct RespData {
            token: String,
        }

        let resp = self
            .http
            .post(format!("{BASE_URL}/v4/login"))
            .json(&Body {
                apikey: &self.api_key,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<Resp>()
            .await?;

        Ok(resp.data.token)
    }

    #[tracing::instrument(skip(self, token), fields(query = %query, kind = %kind))]
    async fn search(&self, token: &str, query: &str, kind: &str) -> AnyResult<Vec<SearchResult>> {
        #[derive(Deserialize)]
        struct Resp {
            data: Option<Vec<SearchResult>>,
        }

        let resp = self
            .http
            .get(format!("{BASE_URL}/v4/search"))
            .bearer_auth(token)
            .query(&[("query", query), ("type", kind)])
            .send()
            .await?
            .error_for_status()?
            .json::<Resp>()
            .await?;

        Ok(resp.data.unwrap_or_default())
    }

    #[tracing::instrument(skip(self, token), fields(tvdb_id = id))]
    async fn series_extended(&self, token: &str, id: i64) -> AnyResult<SeriesExtended> {
        #[derive(Deserialize)]
        struct Resp {
            data: SeriesExtended,
        }

        Ok(self
            .http
            .get(format!("{BASE_URL}/v4/series/{id}/extended"))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json::<Resp>()
            .await?
            .data)
    }

    #[tracing::instrument(skip(self, token), fields(tvdb_id = id))]
    async fn movie_extended(&self, token: &str, id: i64) -> AnyResult<MovieExtended> {
        #[derive(Deserialize)]
        struct Resp {
            data: MovieExtended,
        }

        Ok(self
            .http
            .get(format!("{BASE_URL}/v4/movies/{id}/extended"))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json::<Resp>()
            .await?
            .data)
    }
}

// ── API response types ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchResult {
    /// Numeric TVDB ID as a string (e.g. "12345").
    tvdb_id: Option<String>,
    /// Release year as a string (e.g. "2024").
    year: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesExtended {
    name: String,
    first_aired: Option<NaiveDate>,
    remote_ids: Option<Vec<RemoteId>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MovieExtended {
    name: String,
    /// Release year as a string (e.g. "2010") — TVDB returns this as a string, not an integer.
    year: Option<String>,
    /// Fallback: array of release records each with a `date` field.
    releases: Option<Vec<MovieRelease>>,
    remote_ids: Option<Vec<RemoteId>>,
}

#[derive(Deserialize)]
struct MovieRelease {
    date: Option<NaiveDate>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteId {
    id: String,
    source_name: String,
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Load all entries from a sled tree into a `HashMap`.
/// Entries that fail to deserialize are logged as warnings and skipped.
fn load_tree<V>(tree: &sled::Tree, label: &str) -> HashMap<CacheKey, V>
where
    V: for<'de> Deserialize<'de>,
{
    let mut map = HashMap::new();
    for result in tree {
        match result {
            Err(e) => {
                tracing::warn!(label, error = ?e, "error reading cache tree entry");
            }
            Ok((k, v)) => {
                let key = serde_json::from_slice::<CacheKey>(&k);
                let val = serde_json::from_slice::<V>(&v);
                match (key, val) {
                    (Ok(key), Ok(val)) => {
                        map.insert(key, val);
                    }
                    _ => {
                        tracing::warn!(label, "skipping unreadable cache entry");
                    }
                }
            }
        }
    }
    tracing::info!(label, count = map.len(), "loaded cache entries from disk");
    map
}

/// Serialize `key` + `value` and insert into `tree`.
/// Errors are logged and swallowed — persistence failures are non-fatal.
fn persist_entry<V: Serialize>(tree: &sled::Tree, key: &CacheKey, value: &V, label: &str) {
    let k = match serde_json::to_vec(key) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(label, error = ?e, "failed to serialize cache key");
            return;
        }
    };
    let v = match serde_json::to_vec(value) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(label, error = ?e, "failed to serialize cache value");
            return;
        }
    };
    if let Err(e) = tree.insert(k, v) {
        tracing::error!(label, error = ?e, "failed to persist cache entry");
    }
}

fn pick_result(results: &[SearchResult], year_hint: Option<u32>) -> Option<&SearchResult> {
    if results.is_empty() {
        return None;
    }
    if let Some(year) = year_hint {
        let y = year.to_string();
        if let Some(r) = results
            .iter()
            .find(|r| r.year.as_deref() == Some(y.as_str()))
        {
            return Some(r);
        }
    }
    results.first()
}

fn parse_tvdb_id(s: Option<&String>) -> AnyResult<i64> {
    s.map(String::as_str)
        .ok_or_else(|| eyre::eyre!("search result missing tvdb_id"))?
        .parse::<i64>()
        .map_err(|e| eyre::eyre!("invalid tvdb_id: {e}"))
}

fn extract_imdb(remote_ids: Option<&[RemoteId]>) -> Option<String> {
    remote_ids?
        .iter()
        .find(|r| r.source_name == "IMDB")
        .map(|r| r.id.clone())
}

/// Retry `f` up to `attempts` times, sleeping `delay` between failures.
/// The last error is returned if all attempts are exhausted.
async fn retry<F, Fut, T>(attempts: u32, delay: Duration, operation: &str, f: F) -> AnyResult<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = AnyResult<T>>,
{
    for attempt in 1..=attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) if attempt < attempts => {
                tracing::warn!(
                    operation,
                    attempt,
                    attempts,
                    delay_ms = delay.as_millis(),
                    error = ?e,
                    "TVDB request failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TvdbConfig;

    fn client_from_env() -> Client {
        dotenvy::dotenv().ok();
        let api_key =
            std::env::var("VIDSORT_TVDB_API_KEY").expect("VIDSORT_TVDB_API_KEY must be set");
        Client::new(&TvdbConfig {
            tvdb_api_key: api_key,
            retry_attempts: 3,
            retry_delay: std::time::Duration::from_secs(1).into(),
            cache_path: None,
        })
        .expect("failed to create TVDB client")
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    #[ignore = "requires live TVDB API"]
    fn enrich_show_harrow() {
        rt().block_on(async {
            let client = client_from_env();
            let meta = client
                .enrich_show("Harrow", Some(2018), 2, 10)
                .await
                .unwrap();
            println!("{meta:#?}");
            let meta = meta.expect("expected a match");
            assert!(!meta.canonical_title.is_empty());
            assert_eq!(meta.season, 2);
            assert_eq!(meta.episode, 10);
        });
    }

    #[test]
    #[ignore = "requires live TVDB API"]
    fn enrich_show_no_year_hint() {
        rt().block_on(async {
            let client = client_from_env();
            let meta = client.enrich_show("Being Human", None, 1, 1).await.unwrap();
            println!("{meta:#?}");
            assert!(meta.is_some());
        });
    }

    #[test]
    #[ignore = "requires live TVDB API"]
    fn enrich_movie_inception() {
        rt().block_on(async {
            let client = client_from_env();
            let meta = client.enrich_movie("Inception", Some(2010)).await.unwrap();
            println!("{meta:#?}");
            let meta = meta.expect("expected a match");
            assert!(!meta.canonical_title.is_empty());
            assert_eq!(meta.release_year, Some(2010));
            assert!(meta.imdb_id.as_deref().unwrap_or("").starts_with("tt"));
        });
    }

    #[test]
    #[ignore = "requires live TVDB API"]
    fn enrich_movie_alien_1979() {
        rt().block_on(async {
            let client = client_from_env();
            let meta = client.enrich_movie("Alien", Some(1979)).await.unwrap();
            println!("{meta:#?}");
            let meta = meta.expect("expected a match");
            assert_eq!(meta.release_year, Some(1979));
        });
    }
}

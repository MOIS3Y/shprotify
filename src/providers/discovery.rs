//! Read-only recommendation adapters with persistent normalized-response
//! cache.

use super::Transport;
use crate::{
    config::DiscoveryConfig,
    db,
    discovery::{
        CandidateData, CandidateKind, Credit, FeedRequest, GeneratedFeed,
        Suggestion,
    },
    library::enrichment::mbid,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

/// Normalized provider collection; no credentials or arbitrary response
/// fields.
#[derive(Deserialize, Serialize)]
struct Collection {
    /// Human-readable provider title.
    title: String,
    /// Public source page; not an audio source.
    source_url: Option<String>,
    /// Validated bounded candidates in original source order.
    items: Vec<CandidateData>,
    /// Source exceeded the bounded import window.
    #[serde(default)]
    truncated: bool,
}

/// Reusable adapter client with operator settings and shared rate limiting.
#[derive(Clone)]
pub(crate) struct DiscoveryProviders {
    /// Storage for responses shared across users without personal decisions.
    pool: SqlitePool,
    /// Configured integration switches and application key, never logged.
    config: DiscoveryConfig,
    /// HTTP transport exists only when external discovery is enabled.
    http: Option<Transport>,
    /// Production endpoints are fixed; tests replace them with local mocks.
    lastfm: String,
    /// Public ListenBrainz API base.
    listenbrainz: String,
    /// MusicBrainz metadata search base.
    musicbrainz: String,
}

impl DiscoveryProviders {
    /// Initialize adapters without network requests or user history export.
    pub(crate) fn new(
        pool: SqlitePool,
        config: DiscoveryConfig,
    ) -> Result<Self> {
        let http = if config.enabled {
            Some(Transport::new(&config.user_agent)?)
        } else {
            None
        };
        Ok(Self {
            pool,
            config,
            http,
            lastfm: "https://ws.audioscrobbler.com/2.0/".into(),
            listenbrainz: "https://api.listenbrainz.org/1".into(),
            musicbrainz: "https://musicbrainz.org/ws/2".into(),
        })
    }

    /// Public playlist search can complement Last.fm without a user account.
    pub(crate) fn playlists_enabled(&self) -> bool {
        self.config.enabled && self.config.listenbrainz_enabled
    }

    /// Last.fm is optional; personal feeds can remain entirely local.
    pub(crate) fn lastfm_enabled(&self) -> bool {
        self.config.enabled && !self.config.lastfm_api_key.is_empty()
    }

    /// Validate configured capabilities before persisting a new external job.
    pub(crate) fn supports(
        &self,
        request: &FeedRequest,
    ) -> Result<(), &'static str> {
        match request {
            FeedRequest::Personal {} | FeedRequest::Rediscover {} => Ok(()),
            FeedRequest::NewReleases {}
                if self.config.enabled && self.config.musicbrainz_enabled =>
            {
                Ok(())
            }
            FeedRequest::Playlist { .. }
            | FeedRequest::PlaylistSearch { .. }
                if self.config.enabled && self.config.listenbrainz_enabled =>
            {
                Ok(())
            }
            FeedRequest::NewReleases {}
            | FeedRequest::Playlist { .. }
            | FeedRequest::PlaylistSearch { .. } => {
                Err("Requested discovery integration is disabled")
            }
            _ if self.lastfm_enabled() => Ok(()),
            _ => {
                Err("Last.fm discovery is disabled or has no application key")
            }
        }
    }

    /// Fetch a direct chart/search/playlist request through its own adapter.
    pub(crate) async fn fetch(
        &self,
        request: &FeedRequest,
    ) -> Result<GeneratedFeed> {
        let (provider, params, title, url) = match request {
            FeedRequest::Chart {} => (
                "lastfm",
                json!({"method":"chart.getTopTracks"}),
                "Last.fm global chart".into(),
                Some("https://www.last.fm/charts".into()),
            ),
            FeedRequest::Country { country } => (
                "lastfm",
                json!({"method":"geo.getTopTracks","country":country}),
                format!("Last.fm chart: {country}"),
                None,
            ),
            FeedRequest::Tag { tag } => (
                "lastfm",
                json!({"method":"tag.getTopTracks","tag":tag}),
                format!("Last.fm tag: {tag}"),
                None,
            ),
            FeedRequest::SimilarTrack { artist, track } => (
                "lastfm",
                json!({"method":"track.getSimilar","artist":artist,
                    "track":track,"autocorrect":"1"}),
                format!("Similar to {artist} — {track}"),
                None,
            ),
            FeedRequest::Search { query, artist } => {
                let mut params =
                    json!({"method":"track.search","track":query});
                if let Some(artist) = artist {
                    params["artist"] = json!(artist);
                }
                ("lastfm", params, format!("Search: {query}"), None)
            }
            FeedRequest::PlaylistSearch { query } => (
                "listenbrainz-search",
                json!({"query":query}),
                format!("Public playlists: {query}"),
                None,
            ),
            FeedRequest::Playlist { playlist_id } => (
                "listenbrainz",
                json!({"playlist_id":playlist_id}),
                "ListenBrainz playlist".into(),
                Some(format!(
                    "https://listenbrainz.org/playlist/{playlist_id}"
                )),
            ),
            FeedRequest::SimilarArtist { artist } => {
                return self.similar_artist(artist).await;
            }
            _ => anyhow::bail!("Unsupported direct collection request"),
        };
        let collection = self.collection(provider, params, title, url).await?;
        let reason = format!("From {}", collection.title);
        Ok(generated(collection, &reason))
    }

    /// Resolve similar artists, then take tracks from up to two neighbors.
    async fn similar_artist(&self, artist: &str) -> Result<GeneratedFeed> {
        let params = json!({"method":"artist.getSimilar","artist":artist});
        let key = crate::discovery::digest(&params.to_string());
        let neighbors = if let Some(value) = db::discovery::cached(
            &self.pool,
            "lastfm-artists",
            &key,
            self.config.cache_ttl_seconds,
        )
        .await?
        {
            serde_json::from_value::<Vec<String>>(value)?
        } else {
            let value = self.lastfm_request(&params).await?;
            let values = list(&value["similarartists"]["artist"])?;
            let neighbors = values
                .into_iter()
                .take(2)
                .map(|v| {
                    crate::discovery::text(
                        v["name"].as_str().context("Missing neighbor name")?,
                        255,
                    )
                    .map_err(anyhow::Error::msg)
                })
                .collect::<Result<Vec<_>>>()?;
            db::discovery::cache(
                &self.pool,
                "lastfm-artists",
                &key,
                &json!(neighbors),
                self.config.cache_ttl_seconds,
            )
            .await?;
            neighbors
        };
        let mut items = Vec::new();
        for neighbor in neighbors {
            let collection = self
                .collection(
                    "lastfm",
                    json!({"method":"artist.getTopTracks","artist":neighbor}),
                    format!("Tracks by {neighbor}"),
                    None,
                )
                .await?;
            for candidate in collection.items.into_iter().take(15) {
                items.push(Suggestion {
                    candidate,
                    weight: 2.0,
                    reasons: vec![format!(
                        "Artist related to {artist} on Last.fm"
                    )],
                });
            }
        }
        Ok(GeneratedFeed {
            title: format!("Artists related to {artist}"),
            source_url: None,
            items,
            warnings: vec![],
        })
    }

    /// Fetch editions released in the last 90 days by exact artist UUID.
    pub(crate) async fn new_releases(
        &self,
        artist: &str,
        id: &str,
    ) -> Result<GeneratedFeed> {
        let id = mbid(id).map_err(anyhow::Error::msg)?;
        let date: String = sqlx::query_scalar(
            "SELECT date('now','-90 days') || ' TO ' || date('now')",
        )
        .fetch_one(&self.pool)
        .await?;
        let params = json!({"query":format!("arid:{id} AND date:[{date}]")});
        let collection = self
            .collection(
                "musicbrainz",
                params,
                format!("Recent releases by {artist}"),
                None,
            )
            .await?;
        Ok(generated(
            collection,
            &format!("Released in 90 days: {artist}"),
        ))
    }

    /// Cache only normalized public metadata, never complete upstream
    /// payloads.
    async fn collection(
        &self,
        provider: &str,
        params: Value,
        title: String,
        source_url: Option<String>,
    ) -> Result<Collection> {
        let key = crate::discovery::digest(&params.to_string());
        if let Some(value) = db::discovery::cached(
            &self.pool,
            provider,
            &key,
            self.config.cache_ttl_seconds,
        )
        .await?
        {
            return Ok(serde_json::from_value(value)?);
        }
        let mut collection = match provider {
            "lastfm" => Collection {
                title,
                source_url,
                items: lastfm_tracks(&self.lastfm_request(&params).await?)?,
                truncated: false,
            },
            "listenbrainz" => {
                let id = params["playlist_id"]
                    .as_str()
                    .context("No playlist ID")?;
                let value = self
                    .get_json(
                        &format!("{}/playlist/{id}", self.listenbrainz),
                        &[],
                    )
                    .await?;
                playlist(value, source_url)?
            }
            "listenbrainz-search" => {
                let query =
                    params["query"].as_str().context("Missing query")?;
                let value = self
                    .get_json(
                        &format!("{}/playlist/search", self.listenbrainz),
                        &[("query", query), ("count", "50")],
                    )
                    .await?;
                let mut items = Vec::new();
                for item in list(&value["playlists"])? {
                    let info = &item["playlist"];
                    let identifier = info["identifier"]
                        .as_str()
                        .context("Missing playlist identity")?;
                    let id = identifier
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .context("Missing playlist UUID")?;
                    let id = mbid(id).map_err(anyhow::Error::msg)?;
                    items.push(CandidateData {
                        kind: CandidateKind::Playlist,
                        title: info["title"]
                            .as_str()
                            .context("Missing playlist title")?
                            .into(),
                        artists: vec![],
                        mbid: None,
                        provider: "listenbrainz".into(),
                        external_id: id.clone(),
                        url: Some(format!(
                            "https://listenbrainz.org/playlist/{id}"
                        )),
                        release_date: None,
                    });
                }
                Collection {
                    title,
                    source_url,
                    items,
                    truncated: false,
                }
            }
            "musicbrainz" => {
                let query =
                    params["query"].as_str().context("No release query")?;
                let value = self
                    .get_json(
                        &format!("{}/release", self.musicbrainz),
                        &[("query", query), ("fmt", "json"), ("limit", "50")],
                    )
                    .await?;
                let values = list(&value["releases"])?;
                let mut items = Vec::new();
                for value in values {
                    let parsed = super::musicbrainz::proposal(
                        crate::library::enrichment::EntityKind::Release,
                        value,
                    )?;
                    let id = parsed.mbid.context("Missing release ID")?;
                    items.push(CandidateData {
                        kind: CandidateKind::Release,
                        title: parsed.name.context("Missing release title")?,
                        artists: parsed
                            .artists
                            .into_iter()
                            .map(|a| Credit {
                                name: a.name,
                                mbid: Some(a.mbid),
                            })
                            .collect(),
                        mbid: Some(id.clone()),
                        provider: "musicbrainz".into(),
                        external_id: format!("release:{id}"),
                        url: Some(format!(
                            "https://musicbrainz.org/release/{id}"
                        )),
                        release_date: value["date"]
                            .as_str()
                            .map(str::to_owned),
                    });
                }
                Collection {
                    title,
                    source_url,
                    items,
                    truncated: false,
                }
            }
            _ => anyhow::bail!("Unknown discovery provider"),
        };
        ensure!(
            collection.items.len() <= crate::discovery::MAX_ITEMS,
            "Too many provider candidates"
        );
        collection.title = collection.title.chars().take(255).collect();
        collection.title = crate::discovery::text(&collection.title, 255)
            .map_err(anyhow::Error::msg)?;
        for item in &mut collection.items {
            item.validate().map_err(anyhow::Error::msg)?;
        }
        db::discovery::cache(
            &self.pool,
            provider,
            &key,
            &serde_json::to_value(&collection)?,
            self.config.cache_ttl_seconds,
        )
        .await?;
        Ok(collection)
    }

    /// Form POST keeps the API key out of URLs, logs and persistent cache
    /// keys.
    async fn lastfm_request(&self, params: &Value) -> Result<Value> {
        let http = self.http.as_ref().context("Discovery disabled")?;
        let mut form: Vec<(String, String)> = params
            .as_object()
            .context("Invalid Last.fm parameters")?
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    v.as_str().context("Invalid parameter")?.into(),
                ))
            })
            .collect::<Result<_>>()?;
        form.extend([
            ("api_key".into(), self.config.lastfm_api_key.clone()),
            ("format".into(), "json".into()),
            ("limit".into(), "50".into()),
        ]);
        http.pace().await;
        let response = http
            .client
            .post(&self.lastfm)
            .form(&form)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Last.fm connection failed"))?;
        let bytes = Transport::bytes(response, 2 * 1024 * 1024)
            .await
            .map_err(|_| anyhow::anyhow!("Last.fm response failed"))?;
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value.get("error").is_none(),
            "Last.fm API reported an error"
        );
        Ok(value)
    }

    /// Fetch a bounded public response from a fixed adapter endpoint.
    async fn get_json(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<Value> {
        let http = self.http.as_ref().context("Discovery disabled")?;
        http.pace().await;
        let response = http.client.get(url).query(query).send().await?;
        let bytes = Transport::bytes(response, 2 * 1024 * 1024).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// Normalize an array/singleton or explicit empty list, rejecting missing
/// data.
fn list(value: &Value) -> Result<Vec<&Value>> {
    match value {
        Value::Array(values) => {
            ensure!(
                values.len() <= crate::discovery::MAX_ITEMS,
                "Provider list too large"
            );
            Ok(values.iter().collect())
        }
        Value::Object(_) => Ok(vec![value]),
        Value::String(s) if s.is_empty() => Ok(vec![]),
        _ => anyhow::bail!("Missing or invalid provider collection"),
    }
}

/// Track search, charts and similar/top tracks have different JSON envelopes.
fn lastfm_tracks(value: &Value) -> Result<Vec<CandidateData>> {
    let tracks = value
        .pointer("/similartracks/track")
        .or_else(|| value.pointer("/toptracks/track"))
        .or_else(|| value.pointer("/tracks/track"))
        .or_else(|| value.pointer("/results/trackmatches/track"))
        .context("Missing Last.fm tracks")?;
    list(tracks)?
        .into_iter()
        .map(|value| {
            let title = crate::discovery::text(
                value["name"].as_str().context("Missing track title")?,
                255,
            )
            .map_err(anyhow::Error::msg)?;
            let artist = value["artist"]
                .as_str()
                .or_else(|| value["artist"]["name"].as_str())
                .or_else(|| value["artist"]["#text"].as_str())
                .context("Missing artist")?;
            let artist = crate::discovery::text(artist, 255)
                .map_err(anyhow::Error::msg)?;
            let id = value["mbid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(mbid)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let artist_id = value["artist"]["mbid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(mbid)
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let url = value["url"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let external_id = format!(
                "track:{}",
                crate::discovery::digest(&format!(
                    "{}\0{}",
                    artist.to_lowercase(),
                    title.to_lowercase()
                ))
            );
            Ok(CandidateData {
                kind: CandidateKind::Recording,
                title,
                artists: vec![Credit {
                    name: artist,
                    mbid: artist_id,
                }],
                mbid: id,
                provider: "lastfm".into(),
                external_id,
                url,
                release_date: None,
            })
        })
        .collect()
}

/// Decode a public JSPF playlist; unknown IDs remain candidates without audio.
fn playlist(value: Value, source_url: Option<String>) -> Result<Collection> {
    let playlist = &value["playlist"];
    let title = playlist["title"]
        .as_str()
        .context("Missing playlist title")?;
    let mut items = Vec::new();
    let tracks = playlist["track"]
        .as_array()
        .context("Missing playlist track array")?;
    let truncated = tracks.len() > crate::discovery::MAX_ITEMS;
    for track in tracks.iter().take(crate::discovery::MAX_ITEMS) {
        let identifiers = track["identifier"]
            .as_array()
            .context("Missing playlist recording identifiers")?;
        let id = identifiers
            .iter()
            .filter_map(Value::as_str)
            .find_map(|s| {
                s.strip_prefix("https://musicbrainz.org/recording/")
                    .or_else(|| {
                        s.strip_prefix("http://musicbrainz.org/recording/")
                    })
            })
            .context("Playlist item has no recording MBID")?;
        let id = mbid(id).map_err(anyhow::Error::msg)?;
        let artist =
            track["creator"].as_str().context("Missing track creator")?;
        items.push(CandidateData {
            kind: CandidateKind::Recording,
            title: track["title"]
                .as_str()
                .context("Missing track title")?
                .into(),
            artists: vec![Credit {
                name: artist.into(),
                mbid: None,
            }],
            mbid: Some(id.clone()),
            provider: "listenbrainz".into(),
            external_id: format!("recording:{id}"),
            url: Some(format!("https://musicbrainz.org/recording/{id}")),
            release_date: None,
        });
    }
    Ok(Collection {
        title: title.into(),
        source_url,
        items,
        truncated,
    })
}

/// Preserve source order while retaining a bounded relevance contribution.
fn generated(collection: Collection, reason: &str) -> GeneratedFeed {
    GeneratedFeed {
        title: collection.title,
        source_url: collection.source_url,
        items: collection
            .items
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| Suggestion {
                candidate,
                weight: 2.0 / (1.0 + index as f64 / 20.0),
                reasons: vec![reason.into()],
            })
            .collect(),
        warnings: if collection.truncated {
            vec![
                "Source contains more tracks; first 300 occurrences imported"
                    .into(),
            ]
        } else {
            vec![]
        },
    }
}

#[cfg(test)]
mod tests {
    //! Local HTTP fixtures cover API envelopes, cache reuse and public JSPF.
    use super::*;
    use axum::{
        Json, Router,
        extract::{OriginalUri, State},
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// Complete provider fixture, independent of real operator credentials.
    struct Fixture {
        /// Keep the file-backed database alive.
        _dir: tempfile::TempDir,
        /// Public response cache.
        pool: SqlitePool,
        /// Client with test-only endpoint replacement.
        client: DiscoveryProviders,
        /// Number of upstream Last.fm POSTs.
        count: Arc<AtomicUsize>,
        /// Abort mock HTTP serving when the test finishes.
        task: tokio::task::JoinHandle<()>,
    }

    impl Fixture {
        /// Start a mock with Cyrillic, missing-MBID and playlist-search data.
        async fn new() -> Self {
            let (dir, pool) = db::discovery::tests::setup().await;
            let count = Arc::new(AtomicUsize::new(0));
            let app=Router::new().route("/lastfm",post(
                |State(count): State<Arc<AtomicUsize>>,uri: OriginalUri,
                    body: String| async move {
                    assert!(uri.0.query().is_none());
                    assert!(body.contains("api_key=fixture-key"));
                    count.fetch_add(1,Ordering::SeqCst);
                    let track=json!({"name":"Группа крови","mbid":"",
                        "artist":{"name":"Кино"},
                        "url":"https://www.last.fm/music/Kino/Track"});
                    let value=if body.contains("artist.getSimilar") {
                        json!({"similarartists":{
                            "artist":[{"name":"Neighbor"}]}})
                    } else if body.contains("artist.getTopTracks") {
                        json!({"toptracks":{"track":[track]}})
                    } else if body.contains("track.search") {
                        json!({"results":{"trackmatches":{"track":[track]}}})
                    } else if body.contains("track.getSimilar") {
                        json!({"similartracks":{"track":[track]}})
                    } else {json!({"tracks":{"track":[track]}})};
                    Json(value)
                },
            )).route("/lb/playlist/search",get(||async {
                Json(json!({"playlists":[{"playlist":{
                    "title":"Russian rock","identifier":format!(
                        "https://listenbrainz.org/playlist/{}",
                        db::discovery::tests::MBID)}}]}))
            })).route("/lb/playlist/{id}",get(||async {
                Json(json!({"playlist":{"title":"Curated","track":[{
                    "title":"Звезда","creator":"Кино","identifier":[format!(
                        "https://musicbrainz.org/recording/{}",
                        db::discovery::tests::MBID)]}]}}))
            })).route("/mb/release",get(|uri: OriginalUri|async move {
                assert!(uri.0.query().unwrap().contains("arid"));
                Json(json!({"releases":[{
                    "id":db::discovery::tests::MBID,"title":"New edition",
                    "date":"2026-11-01","artist-credit":[{"artist":{
                        "id":db::discovery::tests::MBID,"name":"Кино"}}]}]}))
            })).with_state(count.clone());
            let listener =
                tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let config = DiscoveryConfig {
                enabled: true,
                user_agent: "shprotify/test (test@example.org)".into(),
                lastfm_api_key: "fixture-key".into(),
                listenbrainz_enabled: true,
                musicbrainz_enabled: true,
                ..DiscoveryConfig::default()
            };
            let mut client =
                DiscoveryProviders::new(pool.clone(), config).unwrap();
            client.lastfm = format!("{base}/lastfm");
            client.listenbrainz = format!("{base}/lb");
            client.musicbrainz = format!("{base}/mb");
            Self {
                _dir: dir,
                pool,
                client,
                count,
                task,
            }
        }
    }

    /// Read methods use form POST, normalize missing IDs and share cached
    /// data.
    #[tokio::test]
    async fn reads_lastfm_collections_and_caches_without_credentials() {
        let f = Fixture::new().await;
        let chart = f.client.fetch(&FeedRequest::Chart {}).await.unwrap();
        assert_eq!(chart.items[0].candidate.title, "Группа крови");
        assert!(chart.items[0].candidate.mbid.is_none());
        f.client.fetch(&FeedRequest::Chart {}).await.unwrap();
        assert_eq!(f.count.load(Ordering::SeqCst), 1);
        for request in [
            FeedRequest::Tag {
                tag: "русский рок".into(),
            },
            FeedRequest::Country {
                country: "Russian Federation".into(),
            },
            FeedRequest::Search {
                query: "Звезда".into(),
                artist: None,
            },
            FeedRequest::SimilarTrack {
                artist: "Кино".into(),
                track: "Звезда".into(),
            },
            FeedRequest::SimilarArtist {
                artist: "Кино".into(),
            },
        ] {
            let feed = f.client.fetch(&request).await.unwrap();
            assert!(!feed.items.is_empty());
        }
        let cached: Vec<String> = sqlx::query_scalar(
            "SELECT document FROM discovery_provider_cache",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert!(cached.iter().all(|s| !s.contains("fixture-key")));
        f.task.abort();
        f.pool.close().await;
    }

    /// Public playlist search leads to an importable stable collection UUID.
    #[tokio::test]
    async fn searches_public_playlists_and_imports_their_recording_ids() {
        let f = Fixture::new().await;
        let request = FeedRequest::PlaylistSearch {
            query: "russian rock".into(),
        };
        f.client.supports(&request).unwrap();
        let found = f.client.fetch(&request).await.unwrap();
        assert_eq!(found.items[0].candidate.kind, CandidateKind::Playlist);
        let playlist_id = found.items[0].candidate.external_id.clone();
        let opened = f
            .client
            .fetch(&FeedRequest::Playlist { playlist_id })
            .await
            .unwrap();
        assert_eq!(opened.title, "Curated");
        assert_eq!(
            opened.items[0].candidate.mbid.as_deref(),
            Some(db::discovery::tests::MBID)
        );
        let new = f
            .client
            .new_releases("Кино", db::discovery::tests::MBID)
            .await
            .unwrap();
        assert_eq!(new.items[0].candidate.kind, CandidateKind::Release);
        assert_eq!(
            new.items[0].candidate.release_date.as_deref(),
            Some("2026-11-01")
        );
        f.task.abort();
        f.pool.close().await;
    }

    /// Provider errors never enter cache or expose keys through error chains.
    #[tokio::test]
    async fn rejects_provider_errors_and_expires_cached_responses() {
        let mut f = Fixture::new().await;
        f.client.fetch(&FeedRequest::Chart {}).await.unwrap();
        sqlx::query("UPDATE discovery_provider_cache SET fetched_at=0")
            .execute(&f.pool)
            .await
            .unwrap();
        f.client.fetch(&FeedRequest::Chart {}).await.unwrap();
        assert_eq!(f.count.load(Ordering::SeqCst), 2);
        f.client.lastfm = f.client.lastfm.replace("/lastfm", "/error");
        let error = f
            .client
            .lastfm_request(&json!({"method":"chart.getTopTracks"}))
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("fixture-key"));
        f.task.abort();
        f.pool.close().await;
    }

    /// Explicit empty collections work; malformed/missing data does not erase.
    #[test]
    fn rejects_error_envelopes_and_accepts_supported_lastfm_shapes() {
        assert!(
            lastfm_tracks(&json!({"error":10,"message":"Bad key"})).is_err()
        );
        assert!(
            lastfm_tracks(&json!({"tracks":{"track":[]}}))
                .unwrap()
                .is_empty()
        );
        let single = json!({"results":{"trackmatches":{"track":{
            "name":"Song","artist":"Artist","mbid":""}}}});
        assert_eq!(lastfm_tracks(&single).unwrap().len(), 1);
        assert!(
            lastfm_tracks(&json!({"tracks":{"track":[{
            "name":"Song","artist":"Artist","mbid":"not-a-uuid"}]}}))
            .is_err()
        );
    }
    /// Large curated lists have an explicit bounded prefix, not silent loss.
    #[test]
    fn reports_truncated_public_playlist_imports() {
        let track = json!({"title":"Track","creator":"Artist","identifier":[
            format!("https://musicbrainz.org/recording/{}",
                db::discovery::tests::MBID)]});
        let collection = playlist(
            json!({"playlist":{"title":"Long list",
            "track":vec![track;301]}}),
            None,
        )
        .unwrap();
        assert_eq!(collection.items.len(), 300);
        let feed = generated(collection, "Test");
        assert_eq!(feed.warnings.len(), 1);
    }
}

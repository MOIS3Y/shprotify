//! Supervised private discovery refreshes; HTTP reads never invoke providers.

use crate::{
    config::DiscoveryConfig,
    db::{
        self,
        discovery::{DiscoveryError, DiscoveryJob},
    },
    discovery::{FeedRequest, GeneratedFeed},
    providers::discovery::DiscoveryProviders,
};
use anyhow::Result;
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::JoinSet};

/// Own every accepted refresh until its terminal state or graceful shutdown.
#[derive(Clone)]
pub(crate) struct DiscoveryService {
    /// Catalog/private-feed storage, kept open until workers drain.
    pool: SqlitePool,
    /// Read-only provider adapters and normalized response cache.
    providers: DiscoveryProviders,
    /// Shared admission and shutdown state.
    workers: Arc<Mutex<Workers>>,
    /// Minimum persisted interval between one account's accepted refreshes.
    cooldown: u32,
    /// Explicit refreshes precede future automatic admissions.
    priority: Arc<super::ManualPriority>,
}

/// Serialize admission with shutdown and retain each supervisor task.
#[derive(Default)]
struct Workers {
    /// Reject new work after graceful shutdown starts.
    stopping: bool,
    /// Globally bounded by the persisted one-running-job constraint.
    tasks: JoinSet<()>,
}

impl DiscoveryService {
    /// Initialize idle adapters; no network activity or periodic tasks start.
    pub(crate) fn new(
        pool: SqlitePool,
        config: DiscoveryConfig,
    ) -> Result<Self> {
        let cooldown = config.refresh_interval_seconds;
        Ok(Self {
            providers: DiscoveryProviders::new(pool.clone(), config)?,
            pool,
            workers: Arc::new(Mutex::new(Workers::default())),
            cooldown,
            priority: Arc::new(super::ManualPriority::default()),
        })
    }

    /// Persist ownership/cooldown before scheduling a bounded refresh worker.
    pub(crate) async fn start(
        &self,
        user: i64,
        request: FeedRequest,
    ) -> Result<DiscoveryJob, DiscoveryError> {
        self.priority.manual();
        self.start_inner(user, request, false).await
    }

    /// Refresh an owned feed without suppressing pending manual requests.
    pub(crate) async fn refresh_automatic(
        &self,
        user: i64,
        id: i64,
    ) -> Result<DiscoveryJob, DiscoveryError> {
        let feed = db::discovery::feed(&self.pool, user, id).await?;
        self.start_inner(user, feed.request, true).await
    }

    /// Both triggers reuse durable ownership, cooldown and bounded admission.
    async fn start_inner(
        &self,
        user: i64,
        mut request: FeedRequest,
        automatic: bool,
    ) -> Result<DiscoveryJob, DiscoveryError> {
        request.validate().map_err(DiscoveryError::Invalid)?;
        self.providers
            .supports(&request)
            .map_err(DiscoveryError::Invalid)?;
        let mut workers = self.workers.lock().await;
        if automatic && !self.priority.automatic_allowed() {
            return Err(DiscoveryError::Conflict);
        }
        if workers.stopping {
            return Err(DiscoveryError::Conflict);
        }
        while let Some(result) = workers.tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error,"Discovery supervisor failed");
            }
        }
        let job =
            db::discovery::admit(&self.pool, user, &request, self.cooldown)
                .await?;
        let response = DiscoveryJob {
            id: job.id,
            feed_id: job.feed_id,
            status: job.status.clone(),
            created_at: job.created_at,
            finished_at: job.finished_at,
            message: job.message.clone(),
        };
        let service = self.clone();
        workers.tasks.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(120),
                service.generate(user, &request),
            )
            .await;
            let result = match result {
                Ok(Ok(generated)) => {
                    db::discovery::complete(&service.pool, &job, generated)
                        .await
                }
                // Avoid recording URLs or user seed text in error logs.
                _ => Err(DiscoveryError::Invalid("Discovery provider failed")),
            };
            if result.is_err() {
                tracing::warn!(job_id = job.id, "Discovery refresh failed");
                if let Err(error) =
                    db::discovery::fail(&service.pool, job.id).await
                {
                    tracing::error!(job_id=job.id,%error,
                        "Failed to persist discovery refresh failure");
                }
            }
        });
        Ok(response)
    }

    /// Refresh a private collection without letting clients alter its owner.
    pub(crate) async fn refresh(
        &self,
        user: i64,
        id: i64,
    ) -> Result<DiscoveryJob, DiscoveryError> {
        let feed = db::discovery::feed(&self.pool, user, id).await?;
        self.start(user, feed.request).await
    }

    /// Assemble bounded candidates and keep external failures out of local
    /// use.
    async fn generate(
        &self,
        user: i64,
        request: &FeedRequest,
    ) -> Result<GeneratedFeed> {
        if !matches!(
            request,
            FeedRequest::Personal {}
                | FeedRequest::Rediscover {}
                | FeedRequest::NewReleases {}
        ) {
            return self.providers.fetch(request).await;
        }
        let profile = db::taste::profile(&self.pool, user).await?;
        if matches!(request, FeedRequest::NewReleases {}) {
            let mut generated = GeneratedFeed {
                title: "Recent releases".into(),
                source_url: None,
                items: vec![],
                warnings: vec![],
            };
            for artist in
                profile.artists.iter().filter(|a| a.mbid.is_some()).take(5)
            {
                if let Some(id) = &artist.mbid {
                    match self.providers.new_releases(&artist.name, id).await {
                        Ok(feed) => generated.items.extend(feed.items),
                        Err(_) => generated
                            .warnings
                            .push("Some recent-release lookups failed".into()),
                    }
                }
            }
            if !profile.artists.iter().any(|a| a.mbid.is_some()) {
                generated.warnings.push(
                    "Follow or like artists with confirmed MusicBrainz IDs"
                        .into(),
                );
            }
            generated.warnings.sort();
            generated.warnings.dedup();
            if generated.items.is_empty()
                && !generated.warnings.is_empty()
                && profile.artists.iter().any(|a| a.mbid.is_some())
            {
                anyhow::bail!("Every recent-release lookup failed");
            }
            return Ok(generated);
        }
        let rediscover = matches!(request, FeedRequest::Rediscover {});
        let mut items =
            db::taste::local(&self.pool, user, &profile, rediscover).await?;
        items.truncate(150);
        let mut warnings = Vec::new();
        if !rediscover && self.providers.lastfm_enabled() {
            let mut requests: Vec<_> = profile
                .tracks
                .iter()
                .take(3)
                .map(|s| FeedRequest::SimilarTrack {
                    artist: s.artist.clone(),
                    track: s.title.clone(),
                })
                .collect();
            if let Some(artist) = profile.artists.first() {
                requests.push(FeedRequest::SimilarArtist {
                    artist: artist.name.clone(),
                });
            }
            if let Some(tag) = profile.preferences.tags.first() {
                requests.push(FeedRequest::Tag { tag: tag.clone() });
            }
            requests.push(FeedRequest::Chart {});
            for request in requests {
                match self.providers.fetch(&request).await {
                    Ok(feed) => items.extend(feed.items),
                    Err(_) => warnings.push(
                        "Some Last.fm sources failed; available results kept"
                            .into(),
                    ),
                }
            }
        }
        if !rediscover && self.providers.playlists_enabled() {
            let query = profile
                .preferences
                .tags
                .first()
                .map(String::as_str)
                .or_else(|| profile.artists.first().map(|a| a.name.as_str()));
            if let Some(query) = query.filter(|q| q.chars().count() >= 3) {
                match self
                    .providers
                    .fetch(&FeedRequest::PlaylistSearch {
                        query: query.into(),
                    })
                    .await
                {
                    Ok(feed) => items.extend(feed.items),
                    Err(_) => {
                        warnings.push("Public playlist search failed".into())
                    }
                }
            }
        }
        warnings.sort();
        warnings.dedup();
        // Rank before capping, so a large local pool cannot crowd out external
        // results. Read-time ranking also honors newer likes/hides
        // immediately.
        items.sort_by(|a, b| {
            let affinity = |item: &crate::discovery::Suggestion| {
                crate::discovery::ranking::affinity(
                    &profile,
                    &item.candidate.artists,
                )
                .map_or(0.0, |a| (a.weight / 8.0).min(4.0))
            };
            (b.weight + affinity(b))
                .total_cmp(&(a.weight + affinity(a)))
                .then_with(|| {
                    a.candidate.identity().cmp(&b.candidate.identity())
                })
        });
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(item.candidate.identity()));
        items = crate::discovery::ranking::diversify(items, |item| {
            item.candidate.artists.first().map_or_else(
                || item.candidate.identity(),
                |a| a.name.to_lowercase(),
            )
        });
        items.truncate(crate::discovery::MAX_ITEMS);
        Ok(GeneratedFeed {
            title: if rediscover {
                "Rediscover your favorites"
            } else {
                "Personal discoveries"
            }
            .into(),
            source_url: None,
            items,
            warnings,
        })
    }

    /// Seal admission and await every accepted refresh before pool closure.
    pub(crate) async fn close(&self) {
        let mut workers = self.workers.lock().await;
        workers.stopping = true;
        while let Some(result) = workers.tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error,"Discovery shutdown worker failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Persisted offline generation and supervised refresh lifecycle.
    use super::*;

    /// Work continues after admission, drains on shutdown and preserves IDs.
    #[tokio::test]
    async fn completes_offline_refresh_and_rejects_work_after_shutdown() {
        let (dir, pool) = db::discovery::tests::setup().await;
        let service =
            DiscoveryService::new(pool.clone(), DiscoveryConfig::default())
                .unwrap();
        let job = service.start(1, FeedRequest::Personal {}).await.unwrap();
        service.close().await;
        assert_eq!(
            db::discovery::job(&pool, 1, job.id).await.unwrap().status,
            "completed"
        );
        let first = db::discovery::contents(&pool, 1, job.feed_id)
            .await
            .unwrap();
        assert_eq!(first.0.version, 1);
        assert_eq!(first.1.len(), 3);
        assert!(matches!(
            service.start(1, FeedRequest::Personal {}).await,
            Err(DiscoveryError::Conflict)
        ));
        pool.close().await;
        let pool =
            db::connect(&dir.path().join("discovery.db")).await.unwrap();
        db::discovery::recover(&pool).await.unwrap();
        let after = db::discovery::contents(&pool, 1, job.feed_id)
            .await
            .unwrap();
        assert_eq!(
            after.1.iter().map(|i| i.id).collect::<Vec<_>>(),
            first.1.iter().map(|i| i.id).collect::<Vec<_>>()
        );
        pool.close().await;
    }
}

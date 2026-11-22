//! Durable private discovery feeds, decisions and provider response cache.

use crate::discovery::{
    CandidateData, CandidateKind, Credit, FeedRequest, Feedback,
    GeneratedFeed, Recommendation,
};
use crate::library::reactions::Reaction;
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool, types::Json};
use utoipa::ToSchema;

/// Expected personal discovery failures without leaking another owner's IDs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoveryError {
    /// Missing resources and resources belonging to another account share 404.
    #[error("Resource not found")]
    NotFound,
    /// Stale edit, occupied worker or refresh cooldown.
    #[error("Discovery changed, is busy, or is cooling down; reload later")]
    Conflict,
    /// Invalid domain input or disabled provider.
    #[error("{0}")]
    Invalid(&'static str),
    /// Unexpected SQLite diagnostics stay in server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
    /// Serialization errors are internal, never returned as provider messages.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A private persistent collection, independent of ordinary user playlists.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Feed {
    /// Stable feed ID, scoped by owner on every operation.
    pub(crate) id: i64,
    /// Canonical request used for explicit refresh and cache identity.
    #[sqlx(json)]
    pub(crate) request: FeedRequest,
    /// Zero until the first successful generation; changes only on success.
    pub(crate) version: i64,
    /// Descriptive collection title, including source titles where available.
    pub(crate) title: String,
    /// Public source collection URL, if one exists.
    pub(crate) source_url: Option<String>,
    /// Unix seconds of the last successful generation.
    pub(crate) generated_at: Option<i64>,
    /// Safe explanations when personal generation used fewer providers.
    #[sqlx(json)]
    pub(crate) warnings: Vec<String>,
}

/// Persisted account-scoped refresh lifecycle.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct DiscoveryJob {
    /// Job identity, unreadable by other accounts including admins.
    pub(crate) id: i64,
    /// Destination feed; previous successful contents survive failures.
    pub(crate) feed_id: i64,
    /// running/completed/failed/interrupted.
    pub(crate) status: String,
    /// Server admission time in Unix seconds.
    pub(crate) created_at: i64,
    /// Terminal time, otherwise null.
    pub(crate) finished_at: Option<i64>,
    /// Safe failure/recovery message; never a raw request URL or API key.
    pub(crate) message: Option<String>,
}

/// Read an owned feed using the same connection as its content snapshot.
async fn read_feed(
    c: &mut SqliteConnection,
    user: i64,
    id: i64,
) -> Result<Feed, DiscoveryError> {
    sqlx::query_as(
        "SELECT id,request,version,title,source_url,generated_at, \
        warnings FROM discovery_feeds WHERE user_id=? AND id=?",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(c)
    .await?
    .ok_or(DiscoveryError::NotFound)
}

/// Page only this user's saved collection definitions, newest first.
pub(crate) async fn feeds(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Feed>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,request,version,title,source_url,generated_at, \
        warnings FROM discovery_feeds WHERE user_id=? ORDER BY id DESC \
        LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Fetch metadata without triggering network I/O or cache regeneration.
pub(crate) async fn feed(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<Feed, DiscoveryError> {
    read_feed(&mut *pool.acquire().await?, user, id).await
}

/// Start a job with a per-account cooldown and globally unique running worker.
/// Definitions are deduplicated by normalized request, never by display title.
pub(crate) async fn admit(
    pool: &SqlitePool,
    user: i64,
    request: &FeedRequest,
    cooldown: u32,
) -> Result<DiscoveryJob, DiscoveryError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 \
        FROM discovery_jobs WHERE status='running')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let cooling: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 \
        FROM discovery_jobs WHERE user_id=? AND created_at>unixepoch()-?)",
    )
    .bind(user)
    .bind(cooldown)
    .fetch_one(&mut *tx)
    .await?;
    if busy || cooling {
        return Err(DiscoveryError::Conflict);
    }
    let json = serde_json::to_string(request)?;
    let key = crate::discovery::digest(&json);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO discovery_feeds \
        (user_id,request_key,request,title) VALUES (?,?,?,'Discovery') \
        ON CONFLICT(user_id,request_key) DO UPDATE SET request=request \
        RETURNING id",
    )
    .bind(user)
    .bind(key)
    .bind(json)
    .fetch_one(&mut *tx)
    .await?;
    let job = sqlx::query_as(
        "INSERT INTO discovery_jobs (user_id,feed_id) \
        VALUES (?,?) RETURNING id,feed_id,status,created_at, \
        finished_at,message",
    )
    .bind(user)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(job)
}

/// Load an owned job without revealing that another user's ID exists.
pub(crate) async fn job(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<DiscoveryJob, DiscoveryError> {
    sqlx::query_as(
        "SELECT id,feed_id,status,created_at,finished_at,message \
        FROM discovery_jobs WHERE user_id=? AND id=?",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(DiscoveryError::NotFound)
}

/// Page this user's refresh history independently of collection contents.
pub(crate) async fn jobs(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<DiscoveryJob>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,feed_id,status,created_at,finished_at,message \
        FROM discovery_jobs WHERE user_id=? ORDER BY id DESC LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Persist one candidate without assigning it a Recording/Release identity.
async fn store(
    c: &mut SqliteConnection,
    mut data: CandidateData,
) -> Result<i64, DiscoveryError> {
    data.validate().map_err(DiscoveryError::Invalid)?;
    let alias: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT c.id,c.mbid FROM discovery_origins o \
        JOIN discovery_candidates c ON c.id=o.candidate_id \
        WHERE o.provider=? AND o.external_id=?",
    )
    .bind(&data.provider)
    .bind(&data.external_id)
    .fetch_optional(&mut *c)
    .await?;
    let alias = if data.provider == "local"
        && alias
            .as_ref()
            .is_some_and(|(_, known)| known.is_some() && *known != data.mbid)
    {
        sqlx::query(
            "DELETE FROM discovery_origins \
            WHERE provider=? AND external_id=?",
        )
        .bind(&data.provider)
        .bind(&data.external_id)
        .execute(&mut *c)
        .await?;
        None
    } else {
        alias
    };
    if let Some((id, known)) = &alias {
        // Provider identity changes must not steal old feedback silently.
        if known.is_some() && data.mbid.is_some() && *known != data.mbid {
            return Err(DiscoveryError::Invalid("Provider identity changed"));
        }
        // Keep an established canonical UUID when a later response omits it.
        if data.mbid.is_none() {
            data.mbid = known.clone();
        }
        if known.is_none() && data.mbid.is_some() {
            let other: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM discovery_candidates WHERE kind=? AND mbid=?",
            )
            .bind(data.kind.as_str())
            .bind(&data.mbid)
            .fetch_optional(&mut *c)
            .await?;
            // Retain the original alias identity rather than merging
            // decisions.
            if other.is_none() {
                sqlx::query(
                    "UPDATE discovery_candidates \
                    SET mbid=?,document=? WHERE id=?",
                )
                .bind(&data.mbid)
                .bind(Json(&data))
                .bind(id)
                .execute(&mut *c)
                .await?;
            }
        } else {
            sqlx::query(
                "UPDATE discovery_candidates SET document=? WHERE id=?",
            )
            .bind(Json(&data))
            .bind(id)
            .execute(&mut *c)
            .await?;
        }
        return Ok(*id);
    }
    let canonical: Option<i64> = if let Some(mbid) = &data.mbid {
        sqlx::query_scalar(
            "SELECT id FROM discovery_candidates \
            WHERE kind=? AND mbid=?",
        )
        .bind(data.kind.as_str())
        .bind(mbid)
        .fetch_optional(&mut *c)
        .await?
    } else {
        None
    };
    if let Some(id) = canonical {
        sqlx::query(
            "INSERT INTO discovery_origins \
            (provider,external_id,candidate_id) VALUES (?,?,?)",
        )
        .bind(&data.provider)
        .bind(&data.external_id)
        .bind(id)
        .execute(c)
        .await?;
        return Ok(id);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO discovery_candidates \
        (identity_key,kind,mbid,document) VALUES (?,?,?,?) \
        ON CONFLICT(identity_key) DO UPDATE SET document=excluded.document \
        RETURNING id",
    )
    .bind(data.identity())
    .bind(data.kind.as_str())
    .bind(&data.mbid)
    .bind(Json(&data))
    .fetch_one(&mut *c)
    .await?;
    sqlx::query(
        "INSERT INTO discovery_origins \
        (provider,external_id,candidate_id) VALUES (?,?,?)",
    )
    .bind(&data.provider)
    .bind(&data.external_id)
    .bind(id)
    .execute(c)
    .await?;
    Ok(id)
}

/// Commit a whole collection and success status atomically, including empty.
pub(crate) async fn complete(
    pool: &SqlitePool,
    job: &DiscoveryJob,
    generated: GeneratedFeed,
) -> Result<(), DiscoveryError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("DELETE FROM discovery_feed_items WHERE feed_id=?")
        .bind(job.feed_id)
        .execute(&mut *tx)
        .await?;
    let mut seen = std::collections::HashSet::new();
    for item in generated
        .items
        .into_iter()
        .take(crate::discovery::MAX_ITEMS)
    {
        let id = store(&mut tx, item.candidate).await?;
        if !seen.insert(id) {
            continue;
        }
        sqlx::query(
            "INSERT INTO discovery_feed_items \
            (feed_id,candidate_id,position,weight,reasons) VALUES (?,?,?,?,?)",
        )
        .bind(job.feed_id)
        .bind(id)
        .bind(seen.len() as i64)
        .bind(item.weight.clamp(0.0, 5.0))
        .bind(Json(item.reasons))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE discovery_feeds SET version=version+1,title=?, \
        source_url=?,generated_at=unixepoch(),warnings=? WHERE id=?",
    )
    .bind(generated.title)
    .bind(generated.source_url)
    .bind(Json(generated.warnings))
    .bind(job.feed_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE discovery_jobs SET status='completed', \
        finished_at=unixepoch() WHERE id=?",
    )
    .bind(job.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Mark an operation terminal without discarding a previous successful feed.
pub(crate) async fn fail(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE discovery_jobs SET status='failed', \
        finished_at=unixepoch(),message='Discovery refresh failed' \
        WHERE id=? AND status='running'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Make crash interruption explicit; external requests are never auto-
/// replayed.
pub(crate) async fn recover(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE discovery_jobs SET status='interrupted', \
        finished_at=unixepoch(),message='Server stopped before completion' \
        WHERE status='running'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Candidate JSON with private account decisions and per-feed provenance.
#[derive(sqlx::FromRow)]
struct ItemRow {
    /// Discovery identity.
    id: i64,
    /// Normalized candidate data.
    #[sqlx(json)]
    document: CandidateData,
    /// Stored personal version, absent until first edit.
    version: Option<i64>,
    /// Explicit discovery assessment, distinct from current local reaction.
    reaction: Option<Reaction>,
    /// Private visibility decision.
    hidden: Option<bool>,
    /// Private acquisition intent.
    wanted: Option<bool>,
    /// Bounded source relevance in this collection.
    weight: f64,
    /// Per-feed explanation.
    #[sqlx(json)]
    reasons: Vec<String>,
}

/// Read consistent collection version and membership before live decoration.
pub(crate) async fn contents(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<(Feed, Vec<Recommendation>), DiscoveryError> {
    let mut tx = pool.begin().await?;
    let feed = read_feed(&mut tx, user, id).await?;
    let rows = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,i.weight,i.reasons FROM discovery_feed_items i \
        JOIN discovery_candidates c ON c.id=i.candidate_id \
        LEFT JOIN discovery_feedback f ON f.candidate_id=c.id AND f.user_id=? \
        WHERE i.feed_id=? ORDER BY i.position",
    )
    .bind(user)
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let items = decorate(&mut tx, user, rows).await?;
    tx.commit().await?;
    Ok((feed, items))
}

/// Verify visibility through owned feed membership or retained feedback.
async fn visible(
    c: &mut SqliteConnection,
    user: i64,
    id: i64,
) -> Result<(), DiscoveryError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS( \
        SELECT 1 FROM discovery_feed_items i JOIN discovery_feeds f \
          ON f.id=i.feed_id WHERE f.user_id=? AND i.candidate_id=? \
        UNION ALL SELECT 1 FROM discovery_feedback \
          WHERE user_id=? AND candidate_id=?)",
    )
    .bind(user)
    .bind(id)
    .bind(user)
    .bind(id)
    .fetch_one(c)
    .await?;
    if !exists {
        return Err(DiscoveryError::NotFound);
    }
    Ok(())
}

/// Read one candidate only if this account has seen or retained it.
pub(crate) async fn candidate(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<Recommendation, DiscoveryError> {
    let mut tx = pool.begin().await?;
    visible(&mut tx, user, id).await?;
    let row = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,0.0 weight,'[]' reasons FROM discovery_candidates c \
        LEFT JOIN discovery_feedback f ON f.candidate_id=c.id AND f.user_id=? \
        WHERE c.id=?",
    )
    .bind(user)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let value = decorate(&mut tx, user, vec![row])
        .await?
        .pop()
        .ok_or(DiscoveryError::NotFound)?;
    tx.commit().await?;
    Ok(value)
}

/// Compare only exact identities/complete credits; ambiguity stays unresolved.
#[derive(sqlx::FromRow)]
struct LocalMatch {
    /// Local catalog namespace.
    kind: String,
    /// Stable local entity identity.
    id: i64,
    /// Confirmed external identity, if any.
    mbid: Option<String>,
    /// Current title for conservative fallback comparison.
    title: String,
    /// Ordered display credits.
    #[sqlx(json)]
    artists: Vec<Credit>,
    /// Registration is not a claim of current filesystem availability.
    has_audio: bool,
}

/// Decorate all candidates in bounded SQL batches with live local
/// availability.
async fn decorate(
    c: &mut SqliteConnection,
    user: i64,
    rows: Vec<ItemRow>,
) -> Result<Vec<Recommendation>, DiscoveryError> {
    let candidate_ids: Vec<_> = rows.iter().map(|r| r.id).collect();
    let links: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT candidate_id,recording_id FROM discovery_download_links \
        WHERE candidate_id IN (SELECT value FROM json_each(?))",
    )
    .bind(Json(candidate_ids))
    .fetch_all(&mut *c)
    .await?;
    let linked_ids: Vec<_> = links.iter().map(|(_, id)| *id).collect();
    let documents: Vec<_> = rows.iter().map(|r| &r.document).collect();
    let matches: Vec<LocalMatch> = sqlx::query_as(
        "WITH wanted AS (SELECT json_extract(value,'$.title') title, \
          json_extract(value,'$.mbid') mbid FROM json_each(?)) \
        SELECT 'recording' kind,r.id,m.mbid,r.title, \
          COALESCE((SELECT json_group_array(json_object('name',a.name, \
            'mbid',NULL)) FROM (SELECT a.name FROM recording_artists ra \
            JOIN artists a ON a.id=ra.artist_id WHERE ra.recording_id=r.id \
            ORDER BY ra.position) a),'[]') artists, \
          EXISTS(SELECT 1 FROM audio_files f WHERE f.recording_id=r.id \
            AND NOT EXISTS( \
                SELECT 1 FROM file_removals d WHERE d.file_id=f.id \
                AND d.status='pending')) \
            has_audio FROM recordings r LEFT JOIN catalog_metadata m \
          ON m.entity_type='recording' AND m.entity_id=r.id \
        WHERE r.title COLLATE NOCASE IN (SELECT title FROM wanted) \
          OR m.mbid IN (SELECT mbid FROM wanted) \
          OR r.id IN (SELECT value FROM json_each(?)) \
        UNION ALL SELECT 'release',r.id,m.mbid,r.title, \
          COALESCE((SELECT json_group_array(json_object('name',a.name, \
            'mbid',NULL)) FROM (SELECT a.name FROM release_artists ra \
            JOIN artists a ON a.id=ra.artist_id WHERE ra.release_id=r.id \
            ORDER BY ra.position) a),'[]'), \
          (EXISTS(SELECT 1 FROM release_tracks t WHERE t.release_id=r.id) \
            AND NOT EXISTS(SELECT 1 FROM release_tracks t \
              WHERE t.release_id=r.id AND NOT EXISTS(SELECT 1 \
                FROM audio_files f WHERE f.recording_id=t.recording_id))) \
        FROM releases r LEFT JOIN catalog_metadata m \
          ON m.entity_type='release' AND m.entity_id=r.id \
        WHERE r.title COLLATE NOCASE IN (SELECT title FROM wanted) \
          OR m.mbid IN (SELECT mbid FROM wanted)",
    )
    .bind(Json(documents))
    .bind(Json(linked_ids))
    .fetch_all(&mut *c)
    .await?;
    let ids: Vec<_> = matches
        .iter()
        .filter(|m| m.kind == "recording")
        .map(|m| m.id)
        .collect();
    let reactions: Vec<(i64, Reaction)> = sqlx::query_as(
        "SELECT recording_id,reaction FROM recording_reactions \
        WHERE user_id=? AND recording_id IN (SELECT value FROM json_each(?))",
    )
    .bind(user)
    .bind(Json(ids))
    .fetch_all(c)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let choices: Vec<_> = matches
                .iter()
                .filter(|m| {
                    if m.kind != r.document.kind.as_str() {
                        return false;
                    }
                    if r.document.provider == "local" {
                        return r.document.external_id
                            == format!("{}:{}", m.kind, m.id)
                            && r.document
                                .mbid
                                .as_ref()
                                .is_none_or(|id| m.mbid.as_ref() == Some(id));
                    }
                    if let Some(id) = &r.document.mbid {
                        return m.mbid.as_ref() == Some(id);
                    }
                    !r.document.artists.is_empty()
                        && m.title.to_lowercase()
                            == r.document.title.to_lowercase()
                        && m.artists.iter().map(|a| a.name.to_lowercase()).eq(
                            r.document
                                .artists
                                .iter()
                                .map(|a| a.name.to_lowercase()),
                        )
                })
                .collect();
            let linked = links.iter().find(|(id, _)| *id == r.id).and_then(
                |(_, id)| {
                    matches
                        .iter()
                        .find(|m| m.kind == "recording" && m.id == *id)
                },
            );
            let matched = if linked.is_some() {
                linked
            } else if choices.len() == 1 {
                choices.first().copied()
            } else {
                None
            };
            let reaction = if let Some(local) =
                matched.filter(|m| m.kind == "recording")
            {
                reactions
                    .iter()
                    .find(|(id, _)| *id == local.id)
                    .map(|(_, v)| *v)
            } else {
                r.reaction
            };
            let method = matched.map(|_| {
                if linked.is_some() {
                    "download_selection"
                } else if r.document.provider == "local" {
                    "local"
                } else if r.document.mbid.is_some() {
                    "mbid"
                } else {
                    "exact_metadata"
                }
            });
            Recommendation {
                id: r.id,
                candidate: r.document,
                local_id: matched.map(|m| m.id),
                match_method: method.map(str::to_owned),
                has_registered_audio: matched.is_some_and(|m| m.has_audio),
                reaction,
                feedback: Feedback {
                    version: r.version.unwrap_or(0),
                    reaction: r.reaction,
                    hidden: r.hidden.unwrap_or(false),
                    wanted: r.wanted.unwrap_or(false),
                },
                score: r.weight,
                reasons: r.reasons,
            }
        })
        .collect())
}

/// Persist complete feedback atomically; stale clients cannot lose decisions.
/// A currently matched recording also updates the existing favorites contract.
pub(crate) async fn feedback(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    input: Feedback,
) -> Result<Feedback, DiscoveryError> {
    if input.version < 0 {
        return Err(DiscoveryError::Invalid("Invalid version"));
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    visible(&mut tx, user, id).await?;
    let old: Option<(i64, Option<Reaction>, bool, bool)> = sqlx::query_as(
        "SELECT version,reaction,hidden,wanted FROM discovery_feedback \
        WHERE user_id=? AND candidate_id=?",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (version, reaction, hidden, wanted) =
        old.unwrap_or((0, None, false, false));
    if version != input.version {
        return Err(DiscoveryError::Conflict);
    }
    let row = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,0.0 weight,'[]' reasons FROM discovery_candidates c \
        LEFT JOIN discovery_feedback f ON f.candidate_id=c.id AND f.user_id=? \
        WHERE c.id=?",
    )
    .bind(user)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let item = decorate(&mut tx, user, vec![row])
        .await?
        .pop()
        .ok_or(DiscoveryError::NotFound)?;
    let local_id = item
        .local_id
        .filter(|_| item.candidate.kind == CandidateKind::Recording);
    let local_reaction: Option<Reaction> = if let Some(recording) = local_id {
        sqlx::query_scalar(
            "SELECT reaction FROM recording_reactions \
            WHERE user_id=? AND recording_id=?",
        )
        .bind(user)
        .bind(recording)
        .fetch_optional(&mut *tx)
        .await?
    } else {
        None
    };
    let local_changed = local_id.is_some() && local_reaction != input.reaction;
    if (reaction, hidden, wanted)
        == (input.reaction, input.hidden, input.wanted)
        && !local_changed
    {
        tx.commit().await?;
        return Ok(input);
    }
    sqlx::query(
        "INSERT INTO discovery_feedback \
        (user_id,candidate_id,version,reaction,hidden,wanted,changed_at) \
        VALUES (?,?,1,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
        ON CONFLICT(user_id,candidate_id) DO UPDATE SET version=version+1, \
        reaction=excluded.reaction,hidden=excluded.hidden, \
        wanted=excluded.wanted, \
        changed_at=excluded.changed_at",
    )
    .bind(user)
    .bind(id)
    .bind(input.reaction)
    .bind(input.hidden)
    .bind(input.wanted)
    .execute(&mut *tx)
    .await?;
    if let Some(recording) = local_id.filter(|_| local_changed) {
        if let Some(reaction) = input.reaction {
            sqlx::query(
                "INSERT INTO recording_reactions \
                (user_id,recording_id,reaction,changed_at) \
                VALUES (?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
                ON CONFLICT(user_id,recording_id) DO UPDATE SET \
                reaction=excluded.reaction,changed_at=excluded.changed_at \
                WHERE reaction<>excluded.reaction",
            )
            .bind(user)
            .bind(recording)
            .bind(reaction)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "DELETE FROM recording_reactions \
                WHERE user_id=? AND recording_id=?",
            )
            .bind(user)
            .bind(recording)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Feedback {
        version: version + 1,
        ..input
    })
}

/// Retain acquisition intent independently of refresh membership or
/// visibility.
pub(crate) async fn wishlist(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Recommendation>, DiscoveryError> {
    decisions(
        pool,
        user,
        DecisionFilter {
            reaction: None,
            hidden: None,
            wanted: Some(true),
        },
        limit,
        offset,
    )
    .await
}

/// Filters for retained personal decisions, including old collections.
pub(crate) struct DecisionFilter {
    /// Last explicit discovery assessment, separate from a later native edit.
    pub(crate) reaction: Option<Reaction>,
    /// Review hidden or visible decisions, or both when absent.
    pub(crate) hidden: Option<bool>,
    /// Acquisition-intent filter used by the wishlist route.
    pub(crate) wanted: Option<bool>,
}

/// Page retained decisions even after their original feed drops the item.
pub(crate) async fn decisions(
    pool: &SqlitePool,
    user: i64,
    filter: DecisionFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<Recommendation>, DiscoveryError> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,0.0 weight,'[]' reasons FROM discovery_feedback f \
        JOIN discovery_candidates c ON c.id=f.candidate_id \
        WHERE f.user_id=? AND (? IS NULL OR f.reaction=?) \
        AND (? IS NULL OR f.hidden=?) AND (? IS NULL OR f.wanted=?) \
        ORDER BY f.changed_at DESC,c.id DESC LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(filter.reaction)
    .bind(filter.reaction)
    .bind(filter.hidden)
    .bind(filter.hidden)
    .bind(filter.wanted)
    .bind(filter.wanted)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    let result = decorate(&mut tx, user, rows).await?;
    tx.commit().await?;
    Ok(result)
}

/// Read only unexpired responses; credentials are never part of cache keys.
pub(crate) async fn cached(
    pool: &SqlitePool,
    provider: &str,
    key: &str,
    ttl: u32,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let found: Option<Json<serde_json::Value>> = sqlx::query_scalar(
        "SELECT document FROM discovery_provider_cache \
        WHERE provider=? AND request_key=? AND fetched_at>=unixepoch()-?",
    )
    .bind(provider)
    .bind(key)
    .bind(ttl)
    .fetch_optional(pool)
    .await?;
    Ok(found.map(|Json(value)| value))
}

/// Replace a bounded validated response and remove expired cache entries.
pub(crate) async fn cache(
    pool: &SqlitePool,
    provider: &str,
    key: &str,
    value: &serde_json::Value,
    ttl: u32,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "DELETE FROM discovery_provider_cache \
        WHERE fetched_at<unixepoch()-?",
    )
    .bind(ttl)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO discovery_provider_cache \
        (provider,request_key,document,fetched_at) VALUES (?,?,?,unixepoch()) \
        ON CONFLICT(provider,request_key) DO UPDATE SET \
        document=excluded.document,fetched_at=excluded.fetched_at",
    )
    .bind(provider)
    .bind(key)
    .bind(Json(value))
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Export an owned feed's currently matched recordings into a private
/// playlist.
/// Missing recordings/releases remain in discovery and are counted as skipped.
pub(crate) async fn export_playlist(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    version: i64,
    details: crate::playlists::PlaylistDetails,
) -> Result<(crate::playlists::Playlist, usize, usize), DiscoveryError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let feed = read_feed(&mut tx, user, id).await?;
    if feed.version != version {
        return Err(DiscoveryError::Conflict);
    }
    let rows = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,i.weight,i.reasons FROM discovery_feed_items i \
        JOIN discovery_candidates c ON c.id=i.candidate_id \
        LEFT JOIN discovery_feedback f ON f.candidate_id=c.id AND f.user_id=? \
        WHERE i.feed_id=? ORDER BY i.position",
    )
    .bind(user)
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let items = decorate(&mut tx, user, rows).await?;
    let total = items.len();
    let recordings: Vec<i64> = items
        .into_iter()
        .filter(|i| {
            i.candidate.kind == CandidateKind::Recording
                && !i.feedback.hidden
                && i.reaction != Some(Reaction::Dislike)
        })
        .filter_map(|i| i.local_id)
        .collect();
    if recordings.is_empty() {
        return Err(DiscoveryError::Invalid(
            "Feed has no eligible local recordings",
        ));
    }
    let playlist: crate::playlists::Playlist = sqlx::query_as(
        "INSERT INTO playlists (user_id,name,description) VALUES (?,?,?) \
        RETURNING id,name,description,version,created_at,updated_at,is_public",
    )
    .bind(user)
    .bind(details.name)
    .bind(details.description)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO playlist_tracks \
        (playlist_id,position,recording_id) \
        SELECT ?,CAST(key AS INTEGER)+1,value FROM json_each(?)",
    )
    .bind(playlist.id)
    .bind(Json(&recordings))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((playlist, recordings.len(), total - recordings.len()))
}

/// Only unresolved external likes supplement native recording preferences.
pub(crate) async fn external_likes(
    pool: &SqlitePool,
    user: i64,
) -> Result<Vec<CandidateData>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query_as(
        "SELECT c.id,c.document,f.version,f.reaction, \
        f.hidden,f.wanted,0.0 weight,'[]' reasons FROM discovery_feedback f \
        JOIN discovery_candidates c ON c.id=f.candidate_id \
        WHERE f.user_id=? AND f.reaction='like' AND f.hidden=0 \
        ORDER BY f.changed_at DESC,c.id DESC LIMIT 100",
    )
    .bind(user)
    .fetch_all(&mut *tx)
    .await?;
    let items = decorate(&mut tx, user, rows).await.map_err(|e| match e {
        DiscoveryError::Storage(error) => error,
        error => sqlx::Error::Protocol(error.to_string()),
    })?;
    tx.commit().await?;
    Ok(items
        .into_iter()
        .filter(|i| i.local_id.is_none() && i.reaction == Some(Reaction::Like))
        .map(|i| i.candidate)
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    //! Durable identity, owner privacy, atomic refresh and decision semantics.
    use super::*;
    use crate::discovery::{GeneratedFeed, Suggestion};
    pub(crate) const MBID: &str = "12345678-1234-1234-1234-123456789abc";

    /// Seed independent owners plus two tracks by one Cyrillic-named artist.
    pub(crate) async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&dir.path().join("discovery.db"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id,name) VALUES (1,'alice'),(2,'bob')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO artists (id,name) VALUES (1,'Кино'),(2,'Other')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recordings (id,title) \
            VALUES (1,'Звезда'),(2,'Группа крови'),(3,'Elsewhere')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recording_artists \
            (recording_id,artist_id,position) VALUES (1,1,0),(2,1,0),(3,2,0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE catalog_metadata SET mbid=? \
            WHERE entity_type='recording' AND entity_id=1",
        )
        .bind(MBID)
        .execute(&pool)
        .await
        .unwrap();
        (dir, pool)
    }

    /// One independently identified external recording, without required MBID.
    pub(crate) fn suggestion(
        provider: &str,
        id: &str,
        title: &str,
        mbid: Option<&str>,
    ) -> Suggestion {
        Suggestion {
            candidate: CandidateData {
                kind: CandidateKind::Recording,
                title: title.into(),
                artists: vec![Credit {
                    name: "Кино".into(),
                    mbid: None,
                }],
                mbid: mbid.map(str::to_owned),
                provider: provider.into(),
                external_id: id.into(),
                url: None,
                release_date: None,
            },
            weight: 1.0,
            reasons: vec!["Test source".into()],
        }
    }

    /// Build a complete atomic generation payload from normalized fixtures.
    pub(crate) fn generated(items: Vec<Suggestion>) -> GeneratedFeed {
        GeneratedFeed {
            title: "Test collection".into(),
            source_url: None,
            items,
            warnings: vec![],
        }
    }

    /// Public source data is shared; collection membership and feedback are
    /// not.
    #[tokio::test]
    async fn preserves_candidates_and_private_wishlist_across_refresh_restart()
    {
        let (dir, pool) = setup().await;
        let job = admit(&pool, 1, &FeedRequest::Chart {}, 0).await.unwrap();
        complete(
            &pool,
            &job,
            generated(vec![
                suggestion("lastfm", "one", "Звезда", Some(MBID)),
                suggestion("listenbrainz", "one", "Звезда", Some(MBID)),
                suggestion("lastfm", "unknown", "Новая музыка", None),
            ]),
        )
        .await
        .unwrap();
        let (first, items) = contents(&pool, 1, job.feed_id).await.unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].local_id, Some(1));
        assert_eq!(items[0].match_method.as_deref(), Some("mbid"));
        assert_eq!(items[1].local_id, None);
        let id = items[1].id;
        assert!(matches!(
            candidate(&pool, 2, id).await,
            Err(DiscoveryError::NotFound)
        ));
        assert!(matches!(
            contents(&pool, 2, job.feed_id).await,
            Err(DiscoveryError::NotFound)
        ));
        assert!(matches!(
            super::job(&pool, 2, job.id).await,
            Err(DiscoveryError::NotFound)
        ));
        feedback(
            &pool,
            1,
            id,
            Feedback {
                version: 0,
                reaction: Some(Reaction::Like),
                wanted: true,
                hidden: false,
            },
        )
        .await
        .unwrap();
        let refresh =
            admit(&pool, 1, &FeedRequest::Chart {}, 0).await.unwrap();
        assert_eq!(refresh.feed_id, first.id);
        complete(&pool, &refresh, generated(vec![])).await.unwrap();
        assert_eq!(wishlist(&pool, 1, 50, 0).await.unwrap()[0].id, id);
        assert!(wishlist(&pool, 2, 50, 0).await.unwrap().is_empty());
        let recordings: i64 =
            sqlx::query_scalar("SELECT count(*) FROM recordings")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(recordings, 3);
        pool.close().await;
        let pool = crate::db::connect(&dir.path().join("discovery.db"))
            .await
            .unwrap();
        assert_eq!(candidate(&pool, 1, id).await.unwrap().feedback.version, 1);
        assert_eq!(feed(&pool, 1, first.id).await.unwrap().version, 2);
        pool.close().await;
    }

    /// Live exact matches are conservative and refresh errors retain old data.
    #[tokio::test]
    async fn resolves_exact_credits_and_rolls_back_invalid_generations() {
        let (_dir, pool) = setup().await;
        let job = admit(&pool, 1, &FeedRequest::Chart {}, 0).await.unwrap();
        complete(
            &pool,
            &job,
            generated(vec![suggestion(
                "lastfm",
                "known",
                "Группа крови",
                None,
            )]),
        )
        .await
        .unwrap();
        let (first, items) = contents(&pool, 1, job.feed_id).await.unwrap();
        assert_eq!(items[0].local_id, Some(2));
        assert_eq!(items[0].match_method.as_deref(), Some("exact_metadata"));
        sqlx::query(
            "INSERT INTO recordings (id,title) VALUES (4,'Группа крови')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recording_artists \
            (recording_id,artist_id,position) VALUES (4,1,0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            contents(&pool, 1, job.feed_id).await.unwrap().1[0].local_id,
            None
        );
        let refresh =
            admit(&pool, 1, &FeedRequest::Chart {}, 0).await.unwrap();
        let invalid = suggestion("lastfm", "bad", "", None);
        assert!(
            complete(&pool, &refresh, generated(vec![invalid]))
                .await
                .is_err()
        );
        fail(&pool, refresh.id).await.unwrap();
        let after = contents(&pool, 1, first.id).await.unwrap();
        assert_eq!(after.0.version, 1);
        assert_eq!(after.1.len(), 1);
        pool.close().await;
    }

    /// Explicit candidate decisions keep matched Recording favorites coherent.
    #[tokio::test]
    async fn versions_feedback_and_syncs_existing_favorites() {
        let (_dir, pool) = setup().await;
        let job = admit(&pool, 1, &FeedRequest::Chart {}, 0).await.unwrap();
        complete(
            &pool,
            &job,
            generated(vec![suggestion(
                "lastfm",
                "known",
                "Звезда",
                Some(MBID),
            )]),
        )
        .await
        .unwrap();
        let id = contents(&pool, 1, job.feed_id).await.unwrap().1[0].id;
        let liked = feedback(
            &pool,
            1,
            id,
            Feedback {
                version: 0,
                reaction: Some(Reaction::Like),
                hidden: false,
                wanted: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(liked.version, 1);
        assert_eq!(
            crate::db::reactions::favorites(&pool, 1, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            feedback(&pool, 1, id, liked.clone()).await.unwrap().version,
            1
        );
        // Direct recording reactions remain authoritative over cached likes.
        sqlx::query(
            "UPDATE recording_reactions SET reaction='dislike' \
            WHERE user_id=1 AND recording_id=1",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            candidate(&pool, 1, id).await.unwrap().reaction,
            Some(Reaction::Dislike)
        );
        let neutral = feedback(
            &pool,
            1,
            id,
            Feedback {
                version: 1,
                reaction: None,
                hidden: false,
                wanted: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(neutral.version, 2);
        assert!(
            crate::db::reactions::favorites(&pool, 1, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let (a, b) = tokio::join!(
            feedback(
                &pool,
                1,
                id,
                Feedback {
                    version: 2,
                    reaction: None,
                    hidden: true,
                    wanted: true
                }
            ),
            feedback(
                &pool,
                1,
                id,
                Feedback {
                    version: 2,
                    reaction: None,
                    hidden: false,
                    wanted: false
                }
            )
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            matches!(a, Err(DiscoveryError::Conflict))
                || matches!(b, Err(DiscoveryError::Conflict))
        );
        pool.close().await;
    }

    /// Refresh admission is persisted, rate limited and recovered after
    /// crashes.
    #[tokio::test]
    async fn serializes_admission_recovers_and_exports_only_local_recordings()
    {
        let (_dir, pool) = setup().await;
        let (a, b) = tokio::join!(
            admit(&pool, 1, &FeedRequest::Chart {}, 60),
            admit(&pool, 2, &FeedRequest::Chart {}, 60)
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        recover(&pool).await.unwrap();
        let user = if a.is_ok() { 1 } else { 2 };
        assert!(matches!(
            admit(&pool, user, &FeedRequest::Personal {}, 60).await,
            Err(DiscoveryError::Conflict)
        ));
        let job = admit(&pool, user, &FeedRequest::Chart {}, 0).await.unwrap();
        complete(
            &pool,
            &job,
            generated(vec![
                suggestion("lastfm", "known", "Звезда", Some(MBID)),
                suggestion("lastfm", "new", "Unknown", None),
            ]),
        )
        .await
        .unwrap();
        let (playlist, included, skipped) = export_playlist(
            &pool,
            user,
            job.feed_id,
            1,
            crate::playlists::PlaylistDetails::new("Discoveries", "").unwrap(),
        )
        .await
        .unwrap();
        assert_eq!((included, skipped), (1, 1));
        assert!(!playlist.is_public);
        assert_eq!(
            crate::db::playlists::tracks(&pool, user, playlist.id, 50, 0)
                .await
                .unwrap()
                .items[0]
                .recording_id,
            1
        );
        let (feed, _) = contents(&pool, user, job.feed_id).await.unwrap();
        assert_eq!(feed.version, 1);
        pool.close().await;
    }
}

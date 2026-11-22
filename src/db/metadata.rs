//! Transactional catalog corrections, revision audit and proposal decisions.

use crate::library::{
    artwork::Artwork,
    enrichment::{ApplyCandidate, Correction, EntityKind, Metadata, Proposal},
};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool, types::Json};
use utoipa::ToSchema;

/// Expected edit failures; storage diagnostics remain private.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MetadataError {
    /// Unknown entity, job or candidate.
    #[error("Resource not found")]
    NotFound,
    /// Stale entity version or already decided candidate.
    #[error("Metadata changed or candidate already decided; reload")]
    Conflict,
    /// Invalid credits, external identity conflict, or candidate policy.
    #[error("{0}")]
    Invalid(&'static str),
    /// Unexpected SQLite error.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Joined editable fields before loading ordered credits and artwork.
type MetadataRow = (i64, Option<String>, String, String, Option<String>);
/// Pending suggestion together with its target and optional encoded image.
type CandidateRow = (String, i64, String, Json<Proposal>, Option<Vec<u8>>);
/// Revision attribution and JSON snapshot read from one audit row.
type RevisionRow = (i64, Option<i64>, String, String, Json<serde_json::Value>);

/// Read a consistent metadata representation using the caller's snapshot.
async fn read(
    c: &mut SqliteConnection,
    kind: EntityKind,
    id: i64,
) -> Result<Metadata, MetadataError> {
    let row: Option<MetadataRow> = sqlx::query_as(match kind {
        EntityKind::Artist => {
            "SELECT m.version,m.mbid,m.source,a.name,NULL \
                FROM catalog_metadata m JOIN artists a ON a.id=m.entity_id \
                WHERE m.entity_type='artist' AND m.entity_id=?"
        }
        EntityKind::Release => {
            "SELECT m.version,m.mbid,m.source,r.title,r.kind \
                FROM catalog_metadata m JOIN releases r ON r.id=m.entity_id \
                WHERE m.entity_type='release' AND m.entity_id=?"
        }
        EntityKind::Recording => {
            "SELECT m.version,m.mbid,m.source,r.title,NULL \
                FROM catalog_metadata m JOIN recordings r \
                ON r.id=m.entity_id WHERE m.entity_type='recording' \
                AND m.entity_id=?"
        }
    })
    .bind(id)
    .fetch_optional(&mut *c)
    .await?;
    let (version, mbid, source, name, release_kind) =
        row.ok_or(MetadataError::NotFound)?;
    let artist_ids = match kind {
        EntityKind::Artist => Vec::new(),
        _ => {
            sqlx::query_scalar(match kind {
                EntityKind::Release => {
                    "SELECT artist_id FROM release_artists \
                WHERE release_id=? ORDER BY position"
                }
                _ => {
                    "SELECT artist_id FROM recording_artists \
                WHERE recording_id=? ORDER BY position"
                }
            })
            .bind(id)
            .fetch_all(&mut *c)
            .await?
        }
    };
    let art: Option<(String, String)> = sqlx::query_as(
        "SELECT digest,source FROM catalog_artwork \
        WHERE entity_type=? AND entity_id=?",
    )
    .bind(kind.as_str())
    .bind(id)
    .fetch_optional(c)
    .await?;
    let (artwork_digest, artwork_source) =
        art.map_or((None, None), |(d, s)| (Some(d), Some(s)));
    let release_kind = release_kind
        .map(|s| {
            serde_json::from_value(serde_json::Value::String(s))
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))
        })
        .transpose()?;
    Ok(Metadata {
        entity_type: kind,
        entity_id: id,
        version,
        name,
        artist_ids,
        release_kind,
        mbid,
        source,
        artwork_digest,
        artwork_source,
    })
}

/// Retrieve current metadata, including live title/credits and cover digest.
pub(crate) async fn get(
    pool: &SqlitePool,
    kind: EntityKind,
    id: i64,
) -> Result<Metadata, MetadataError> {
    let mut tx = pool.begin().await?;
    let result = read(&mut tx, kind, id).await?;
    tx.commit().await?;
    Ok(result)
}

/// Check the optimistic precondition before any side effects.
async fn editable(
    c: &mut SqliteConnection,
    kind: EntityKind,
    id: i64,
    version: i64,
) -> Result<Metadata, MetadataError> {
    let old = read(c, kind, id).await?;
    if old.version != version {
        return Err(MetadataError::Conflict);
    }
    Ok(old)
}

/// Capture the original snapshot once and every subsequent accepted version.
async fn audit(
    c: &mut SqliteConnection,
    value: &Metadata,
    user: Option<i64>,
    source: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO metadata_revisions \
        (entity_type,entity_id,version,user_id,source,snapshot) \
        SELECT ?,?,?,?,?,? WHERE NOT EXISTS \
        (SELECT 1 FROM metadata_revisions WHERE entity_type=? \
        AND entity_id=? AND version=?)",
    )
    .bind(value.entity_type.as_str())
    .bind(value.entity_id)
    .bind(value.version)
    .bind(user)
    .bind(source)
    .bind(Json(value))
    .bind(value.entity_type.as_str())
    .bind(value.entity_id)
    .bind(value.version)
    .execute(c)
    .await?;
    Ok(())
}

/// Apply validated complete fields inside a transaction shared with decisions.
async fn write(
    c: &mut SqliteConnection,
    old: &Metadata,
    input: &Correction,
    user: i64,
    source: &str,
) -> Result<Metadata, MetadataError> {
    let kind = old.entity_type;
    let id = old.entity_id;
    if let Some(mbid) = &input.mbid {
        let duplicate: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM catalog_metadata \
            WHERE entity_type=? AND mbid=? AND entity_id<>?)",
        )
        .bind(kind.as_str())
        .bind(mbid)
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        if duplicate {
            return Err(MetadataError::Invalid(
                "MusicBrainz ID is already linked to another local entity",
            ));
        }
    }
    let missing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM json_each(?) j LEFT JOIN artists a \
        ON a.id=j.value WHERE a.id IS NULL)",
    )
    .bind(Json(&input.artist_ids))
    .fetch_one(&mut *c)
    .await?;
    if missing {
        return Err(MetadataError::Invalid("Unknown artist ID"));
    }
    audit(c, old, None, &old.source).await?;
    sqlx::query(match kind {
        EntityKind::Artist => "UPDATE artists SET name=? WHERE id=?",
        EntityKind::Release => "UPDATE releases SET title=? WHERE id=?",
        EntityKind::Recording => "UPDATE recordings SET title=? WHERE id=?",
    })
    .bind(&input.name)
    .bind(id)
    .execute(&mut *c)
    .await?;
    if let Some(release_kind) = input.release_kind {
        sqlx::query("UPDATE releases SET kind=? WHERE id=?")
            .bind(release_kind)
            .bind(id)
            .execute(&mut *c)
            .await?;
    }
    if kind != EntityKind::Artist {
        sqlx::query(match kind {
            EntityKind::Release => {
                "DELETE FROM release_artists WHERE release_id=?"
            }
            _ => "DELETE FROM recording_artists WHERE recording_id=?",
        })
        .bind(id)
        .execute(&mut *c)
        .await?;
        sqlx::query(match kind {
            EntityKind::Release => {
                "INSERT INTO release_artists \
                (release_id,artist_id,position) SELECT ?,value, \
                CAST(key AS INTEGER) FROM json_each(?)"
            }
            _ => {
                "INSERT INTO recording_artists \
                (recording_id,artist_id,position) SELECT ?,value, \
                CAST(key AS INTEGER) FROM json_each(?)"
            }
        })
        .bind(id)
        .bind(Json(&input.artist_ids))
        .execute(&mut *c)
        .await?;
    }
    sqlx::query(
        "UPDATE catalog_metadata SET version=version+1,mbid=?,source=? \
        WHERE entity_type=? AND entity_id=?",
    )
    .bind(&input.mbid)
    .bind(source)
    .bind(kind.as_str())
    .bind(id)
    .execute(&mut *c)
    .await?;
    // A confirmed/corrected identity replaces scanner matching for new paths.
    if kind == EntityKind::Recording {
        sqlx::query("DELETE FROM scan_recording_matches WHERE recording_id=?")
            .bind(id)
            .execute(&mut *c)
            .await?;
        if let Some(mbid) = &input.mbid {
            sqlx::query(
                "INSERT INTO scan_recording_matches \
                (mbid,recording_id) VALUES (?,?)",
            )
            .bind(mbid)
            .bind(id)
            .execute(&mut *c)
            .await?;
        }
    }
    let saved = read(c, kind, id).await?;
    audit(c, &saved, Some(user), source).await?;
    Ok(saved)
}

/// Manual correction never touches files or replaces entity identities.
pub(crate) async fn correct(
    pool: &SqlitePool,
    kind: EntityKind,
    id: i64,
    input: Correction,
    user: i64,
) -> Result<Metadata, MetadataError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old = editable(&mut tx, kind, id, input.version).await?;
    let saved = write(&mut tx, &old, &input, user, "manual").await?;
    tx.commit().await?;
    Ok(saved)
}

/// Stored image payload is kept separate from JSON representations.
#[derive(sqlx::FromRow)]
pub(crate) struct StoredArtwork {
    /// Validated image type.
    pub(crate) mime: String,
    /// SHA-256 used as the entity tag.
    pub(crate) digest: String,
    /// Bounded original encoded bytes.
    pub(crate) data: Vec<u8>,
}

/// Read an image in one SQL snapshot; no external request is made.
pub(crate) async fn artwork(
    pool: &SqlitePool,
    kind: EntityKind,
    id: i64,
) -> Result<Option<StoredArtwork>, sqlx::Error> {
    sqlx::query_as(
        "SELECT mime,digest,data FROM catalog_artwork \
        WHERE entity_type=? AND entity_id=?",
    )
    .bind(kind.as_str())
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Change/delete cover data within the caller's serialized metadata edit.
async fn write_artwork(
    c: &mut SqliteConnection,
    old: &Metadata,
    artwork: Option<Artwork>,
    source: &str,
    user: i64,
) -> Result<Metadata, MetadataError> {
    audit(c, old, None, &old.source).await?;
    if let Some(art) = artwork {
        sqlx::query(
            "INSERT INTO catalog_artwork \
            (entity_type,entity_id,mime,digest,data,source) \
            VALUES (?,?,?,?,?,?) \
            ON CONFLICT(entity_type,entity_id) DO UPDATE SET \
            mime=excluded.mime,digest=excluded.digest,data=excluded.data, \
            source=excluded.source",
        )
        .bind(old.entity_type.as_str())
        .bind(old.entity_id)
        .bind(art.mime)
        .bind(art.digest)
        .bind(art.data)
        .bind(source)
        .execute(&mut *c)
        .await?;
    } else {
        sqlx::query(
            "DELETE FROM catalog_artwork \
            WHERE entity_type=? AND entity_id=?",
        )
        .bind(old.entity_type.as_str())
        .bind(old.entity_id)
        .execute(&mut *c)
        .await?;
    }
    sqlx::query(
        "UPDATE catalog_metadata SET version=version+1 \
        WHERE entity_type=? AND entity_id=?",
    )
    .bind(old.entity_type.as_str())
    .bind(old.entity_id)
    .execute(&mut *c)
    .await?;
    let saved = read(c, old.entity_type, old.entity_id).await?;
    audit(c, &saved, Some(user), source).await?;
    Ok(saved)
}

/// Set/clear artwork with the same version precondition as text edits.
pub(crate) async fn set_artwork(
    pool: &SqlitePool,
    kind: EntityKind,
    id: i64,
    version: i64,
    artwork: Option<Artwork>,
    user: i64,
) -> Result<Metadata, MetadataError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old = editable(&mut tx, kind, id, version).await?;
    let saved =
        write_artwork(&mut tx, &old, artwork, "manual_artwork", user).await?;
    tx.commit().await?;
    Ok(saved)
}

/// Persisted administrative job snapshot, independent of scan admission.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct MetadataJob {
    /// Stable job identity.
    pub(crate) id: i64,
    /// Target namespace.
    pub(crate) entity_type: String,
    /// Target local identity.
    pub(crate) entity_id: i64,
    /// Requested operation.
    pub(crate) action: String,
    /// running/completed/failed/interrupted.
    pub(crate) status: String,
    /// Safe diagnostic; provider errors and secrets never appear here.
    pub(crate) message: Option<String>,
    /// UTC SQLite creation time.
    pub(crate) created_at: String,
    /// Terminal time, absent while running.
    pub(crate) finished_at: Option<String>,
}

/// Proposal without its binary image; image preview uses a separate route.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Candidate {
    /// Stable candidate identity.
    pub(crate) id: i64,
    /// Parent job.
    pub(crate) job_id: i64,
    /// pending/applied/rejected.
    pub(crate) state: String,
    /// Provider data retained for explicit review.
    #[sqlx(json)]
    pub(crate) proposal: Proposal,
}

/// Read one persisted job, without polling an external service.
pub(crate) async fn job(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<MetadataJob>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,entity_type,entity_id,action,status,message, \
        created_at,finished_at FROM metadata_jobs WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Read at most ten suggestions produced by a bounded provider operation.
pub(crate) async fn candidates(
    pool: &SqlitePool,
    job: i64,
) -> Result<Vec<Candidate>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,job_id,state,proposal \
        FROM metadata_candidates WHERE job_id=? ORDER BY id",
    )
    .bind(job)
    .fetch_all(pool)
    .await
}

/// Atomically retain every candidate before publishing successful completion.
pub(crate) async fn finish(
    pool: &SqlitePool,
    id: i64,
    proposals: Vec<(Proposal, Option<Vec<u8>>)>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for (proposal, artwork) in proposals {
        sqlx::query(
            "INSERT INTO metadata_candidates \
            (job_id,proposal,artwork) VALUES (?,?,?)",
        )
        .bind(id)
        .bind(Json(proposal))
        .bind(artwork)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE metadata_jobs SET status='completed', \
        finished_at=CURRENT_TIMESTAMP WHERE id=? AND status='running'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Startup makes crash interruption explicit; no external work is replayed.
pub(crate) async fn recover(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE metadata_jobs SET status='interrupted', \
        message='Server stopped before completion', \
        finished_at=CURRENT_TIMESTAMP WHERE status='running'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Resolve one candidate and its target in an existing write transaction.
async fn candidate(
    c: &mut SqliteConnection,
    id: i64,
) -> Result<(EntityKind, i64, Proposal, Option<Vec<u8>>), MetadataError> {
    let row: Option<CandidateRow> = sqlx::query_as(
        "SELECT j.entity_type,j.entity_id,c.state, \
            c.proposal,c.artwork FROM metadata_candidates c \
            JOIN metadata_jobs j ON j.id=c.job_id WHERE c.id=?",
    )
    .bind(id)
    .fetch_optional(c)
    .await?;
    let (kind, target, state, Json(proposal), art) =
        row.ok_or(MetadataError::NotFound)?;
    if state != "pending" {
        return Err(MetadataError::Conflict);
    }
    let kind = match kind.as_str() {
        "artist" => EntityKind::Artist,
        "release" => EntityKind::Release,
        _ => EntityKind::Recording,
    };
    Ok((kind, target, proposal, art))
}

/// Reject a pending candidate permanently; a fresh job can offer it again.
pub(crate) async fn reject(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), MetadataError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    candidate(&mut tx, id).await?;
    sqlx::query("UPDATE metadata_candidates SET state='rejected' WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Confirm only selected fields; credit identities never match on name alone.
pub(crate) async fn apply(
    pool: &SqlitePool,
    id: i64,
    policy: ApplyCandidate,
    user: i64,
) -> Result<Metadata, MetadataError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let (kind, target, proposal, art) = candidate(&mut tx, id).await?;
    let old = editable(&mut tx, kind, target, policy.version).await?;
    let saved = if let Some(data) = art {
        if policy.name || policy.credits {
            return Err(MetadataError::Invalid(
                "Artwork candidates require name=false and credits=false",
            ));
        }
        let mime = match proposal.artwork_mime.as_deref() {
            Some("image/png") => "image/png",
            Some("image/jpeg") => "image/jpeg",
            _ => return Err(MetadataError::Invalid("Invalid artwork type")),
        };
        let digest = proposal
            .artwork_digest
            .clone()
            .ok_or(MetadataError::Invalid("Missing artwork digest"))?;
        write_artwork(
            &mut tx,
            &old,
            Some(Artwork { data, mime, digest }),
            &proposal.source,
            user,
        )
        .await?
    } else {
        let mut input = Correction {
            version: policy.version,
            name: old.name.clone(),
            artist_ids: old.artist_ids.clone(),
            release_kind: old.release_kind,
            mbid: proposal.mbid.clone(),
        };
        if policy.name {
            input.name = proposal
                .name
                .clone()
                .ok_or(MetadataError::Invalid("Missing candidate name"))?;
        }
        if policy.credits {
            if kind == EntityKind::Artist {
                return Err(MetadataError::Invalid("Artist has no credits"));
            }
            input.artist_ids.clear();
            for artist in &proposal.artists {
                let found: Option<i64> = sqlx::query_scalar(
                    "SELECT entity_id FROM catalog_metadata \
                    WHERE entity_type='artist' AND mbid=?",
                )
                .bind(&artist.mbid)
                .fetch_optional(&mut *tx)
                .await?;
                let artist_id =
                    if let Some(id) = found {
                        id
                    } else {
                        let id: i64 = sqlx::query_scalar(
                        "INSERT INTO artists (name) VALUES (?) RETURNING id",
                    ).bind(&artist.name).fetch_one(&mut *tx).await?;
                        sqlx::query(
                            "UPDATE catalog_metadata SET mbid=?, \
                        source=? WHERE entity_type='artist' AND entity_id=?",
                        )
                        .bind(&artist.mbid)
                        .bind(&proposal.source)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                        id
                    };
                if !input.artist_ids.contains(&artist_id) {
                    input.artist_ids.push(artist_id);
                }
            }
        }
        input.validate(kind).map_err(MetadataError::Invalid)?;
        write(&mut tx, &old, &input, user, &proposal.source).await?
    };
    sqlx::query("UPDATE metadata_candidates SET state='applied' WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(saved)
}

/// Safe preview for an administrator before accepting an artwork candidate.
pub(crate) async fn preview(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<StoredArtwork>, sqlx::Error> {
    sqlx::query_as(
        "SELECT json_extract(proposal,'$.artwork_mime') AS mime, \
        json_extract(proposal,'$.artwork_digest') AS digest,artwork AS data \
        FROM metadata_candidates WHERE id=? AND artwork IS NOT NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Read a bounded audit page; snapshots retain text and image digest, not
/// BLOBs.
pub(crate) async fn revisions(
    pool: &SqlitePool,
    kind: EntityKind,
    id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<serde_json::Value>, sqlx::Error> {
    let rows: Vec<RevisionRow> = sqlx::query_as(
        "SELECT version,user_id,source,created_at,snapshot \
            FROM metadata_revisions WHERE entity_type=? AND entity_id=? \
            ORDER BY id DESC LIMIT ? OFFSET ?",
    )
    .bind(kind.as_str())
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(version, user_id, source, time, Json(snapshot))| {
            serde_json::json!({"version":version,"user_id":user_id,
            "source":source,"created_at":time,"snapshot":snapshot})
        })
        .collect())
}

/// Correct a file/track association atomically, preserving physical bytes.
/// Compare-and-set rejects concurrent reassignment without merging recordings.
pub(crate) async fn relink(
    pool: &SqlitePool,
    kind: crate::library::enrichment::LinkKind,
    id: i64,
    input: crate::library::enrichment::LinkCorrection,
    user: i64,
) -> Result<(), MetadataError> {
    use crate::library::enrichment::LinkKind;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old: Option<i64> = sqlx::query_scalar(match kind {
        LinkKind::File => "SELECT recording_id FROM audio_files WHERE id=?",
        LinkKind::ReleaseTrack => {
            "SELECT recording_id FROM release_tracks WHERE id=?"
        }
    })
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let old = old.ok_or(MetadataError::NotFound)?;
    if old != input.expected_recording_id {
        return Err(MetadataError::Conflict);
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM recordings WHERE id=?)",
    )
    .bind(input.recording_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(MetadataError::Invalid("Unknown recording ID"));
    }
    if old != input.recording_id {
        sqlx::query(match kind {
            LinkKind::File => {
                "UPDATE audio_files SET recording_id=? WHERE id=?"
            }
            LinkKind::ReleaseTrack => {
                "UPDATE release_tracks SET recording_id=? WHERE id=?"
            }
        })
        .bind(input.recording_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO metadata_link_revisions \
            (link_type,link_id,old_recording_id,new_recording_id,user_id) \
            VALUES (?,?,?,?,?)",
        )
        .bind(match kind {
            LinkKind::File => "file",
            LinkKind::ReleaseTrack => "release_track",
        })
        .bind(id)
        .bind(old)
        .bind(input.recording_id)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Discover persisted jobs after restart or a lost admission response.
pub(crate) async fn jobs(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<MetadataJob>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,entity_type,entity_id,action,status,message, \
        created_at,finished_at FROM metadata_jobs ORDER BY id DESC \
        LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    //! Corrections, confirmed proposals, audit, rollback and crash recovery.
    use super::*;
    use crate::library::enrichment::{ArtistCredit, LinkCorrection, LinkKind};
    const ID: &str = "12345678-1234-1234-1234-123456789abc";

    /// Seed shared entities and one administrator without external services.
    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id,name,role) VALUES (1,'admin','admin')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO artists (id,name) VALUES (1,'Local')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO recordings (id,title) VALUES (1,'Old'),(2,'Two')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    /// A complete valid correction preserves credits unless explicitly
    /// changed.
    fn correction(version: i64, name: &str, id: Option<&str>) -> Correction {
        let mut input = Correction {
            version,
            name: name.into(),
            artist_ids: vec![1],
            release_kind: None,
            mbid: id.map(str::to_owned),
        };
        input.validate(EntityKind::Recording).unwrap();
        input
    }

    /// Persist a synthetic external result using the real completion path.
    async fn propose(
        pool: &SqlitePool,
        target: i64,
        artwork: Option<Artwork>,
    ) -> i64 {
        let job: i64 = sqlx::query_scalar(
            "INSERT INTO metadata_jobs \
            (entity_type,entity_id,action) VALUES ('recording',?,'lookup') \
            RETURNING id",
        )
        .bind(target)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut proposal = Proposal {
            source: "musicbrainz".into(),
            mbid: Some(ID.into()),
            name: Some("External".into()),
            description: None,
            artists: vec![ArtistCredit {
                mbid: "12345678-1234-1234-1234-123456789abd".into(),
                name: "Local".into(),
            }],
            score: Some(100.0),
            artwork_digest: None,
            artwork_mime: None,
        };
        let data = artwork.map(|art| {
            proposal.source = "embedded".into();
            proposal.mbid = None;
            proposal.name = None;
            proposal.artists.clear();
            proposal.artwork_digest = Some(art.digest);
            proposal.artwork_mime = Some(art.mime.into());
            art.data
        });
        finish(pool, job, vec![(proposal, data)]).await.unwrap();
        candidates(pool, job).await.unwrap()[0].id
    }

    /// Manual edits survive opt-in identity linking and duplicate names.
    #[tokio::test]
    async fn applies_selected_fields_and_keeps_complete_revision_history() {
        let (dir, pool) = setup().await;
        let saved = correct(
            &pool,
            EntityKind::Recording,
            1,
            correction(1, "Manual", None),
            1,
        )
        .await
        .unwrap();
        assert_eq!(saved.version, 2);
        let candidate = propose(&pool, 1, None).await;
        assert_eq!(
            get(&pool, EntityKind::Recording, 1).await.unwrap().name,
            "Manual"
        );
        let saved = apply(
            &pool,
            candidate,
            ApplyCandidate {
                version: 2,
                name: false,
                credits: false,
            },
            1,
        )
        .await
        .unwrap();
        assert_eq!(saved.name, "Manual");
        assert_eq!(saved.artist_ids, vec![1]);
        assert_eq!(saved.mbid.as_deref(), Some(ID));
        assert_eq!(saved.version, 3);
        assert!(matches!(
            apply(
                &pool,
                candidate,
                ApplyCandidate {
                    version: 3,
                    name: true,
                    credits: true
                },
                1
            )
            .await,
            Err(MetadataError::Conflict)
        ));
        let candidate = propose(&pool, 1, None).await;
        let saved = apply(
            &pool,
            candidate,
            ApplyCandidate {
                version: 3,
                name: true,
                credits: true,
            },
            1,
        )
        .await
        .unwrap();
        assert_eq!(saved.name, "External");
        assert_ne!(saved.artist_ids, vec![1]);
        // Same display name did not merge with the manually created artist.
        let artists: i64 = sqlx::query_scalar("SELECT count(*) FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artists, 2);
        let scanner: i64 = sqlx::query_scalar(
            "SELECT recording_id FROM scan_recording_matches WHERE mbid=?",
        )
        .bind(ID)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(scanner, 1);
        let audit = revisions(&pool, EntityKind::Recording, 1, 50, 0)
            .await
            .unwrap();
        assert_eq!(audit.len(), 4);
        assert_eq!(audit[3]["snapshot"]["name"], "Old");
        assert_eq!(audit[2]["source"], "manual");
        pool.close().await;
        let pool = crate::db::connect(&dir.path().join("catalog.db"))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, EntityKind::Recording, 1).await.unwrap().version,
            4
        );
        pool.close().await;
    }

    /// Stale clients and conflicting identities cannot partially create data.
    #[tokio::test]
    async fn rejects_conflicts_and_rolls_back_candidate_side_effects() {
        let (_dir, pool) = setup().await;
        correct(
            &pool,
            EntityKind::Recording,
            1,
            correction(1, "First", Some(ID)),
            1,
        )
        .await
        .unwrap();
        let candidate = propose(&pool, 2, None).await;
        assert!(matches!(
            apply(
                &pool,
                candidate,
                ApplyCandidate {
                    version: 1,
                    name: true,
                    credits: true
                },
                1
            )
            .await,
            Err(MetadataError::Invalid(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            get(&pool, EntityKind::Recording, 2).await.unwrap().version,
            1
        );
        reject(&pool, candidate).await.unwrap();
        assert!(matches!(
            reject(&pool, candidate).await,
            Err(MetadataError::Conflict)
        ));
        let (a, b) = tokio::join!(
            correct(
                &pool,
                EntityKind::Recording,
                1,
                correction(2, "A", None),
                1
            ),
            correct(
                &pool,
                EntityKind::Recording,
                1,
                correction(2, "B", None),
                1
            ),
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            matches!(a, Err(MetadataError::Conflict))
                || matches!(b, Err(MetadataError::Conflict))
        );
        pool.close().await;
    }

    /// Covers use the same version as text and retain independent provenance.
    #[tokio::test]
    async fn confirms_artwork_and_does_not_automatically_replace_manual_cover()
    {
        let (_dir, pool) = setup().await;
        let bytes = crate::library::artwork::tests::png();
        let art = crate::library::artwork::validate(bytes.clone()).unwrap();
        let saved =
            set_artwork(&pool, EntityKind::Recording, 1, 1, Some(art), 1)
                .await
                .unwrap();
        assert_eq!(saved.version, 2);
        assert_eq!(saved.artwork_source.as_deref(), Some("manual_artwork"));
        let candidate = propose(
            &pool,
            1,
            Some(crate::library::artwork::validate(bytes.clone()).unwrap()),
        )
        .await;
        assert_eq!(
            preview(&pool, candidate).await.unwrap().unwrap().data,
            bytes
        );
        assert_eq!(
            get(&pool, EntityKind::Recording, 1).await.unwrap().version,
            2
        );
        assert!(matches!(
            apply(
                &pool,
                candidate,
                ApplyCandidate {
                    version: 1,
                    name: false,
                    credits: false
                },
                1
            )
            .await,
            Err(MetadataError::Conflict)
        ));
        let accepted = apply(
            &pool,
            candidate,
            ApplyCandidate {
                version: 2,
                name: false,
                credits: false,
            },
            1,
        )
        .await
        .unwrap();
        assert_eq!(accepted.artwork_source.as_deref(), Some("embedded"));
        set_artwork(&pool, EntityKind::Recording, 1, 3, None, 1)
            .await
            .unwrap();
        assert!(
            artwork(&pool, EntityKind::Recording, 1)
                .await
                .unwrap()
                .is_none()
        );
        pool.close().await;
    }

    /// Crash recovery clears admission and relinks compare the original match.
    #[tokio::test]
    async fn recovers_interrupted_jobs_and_corrects_links_explicitly() {
        let (_dir, pool) = setup().await;
        sqlx::query(
            "INSERT INTO audio_files (id,recording_id,path,format) \
            VALUES (1,1,'/music/source.wav','wav')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_jobs \
            (entity_type,entity_id,action) VALUES ('recording',1,'search')",
        )
        .execute(&pool)
        .await
        .unwrap();
        recover(&pool).await.unwrap();
        assert_eq!(
            job(&pool, 1).await.unwrap().unwrap().status,
            "interrupted"
        );
        relink(
            &pool,
            LinkKind::File,
            1,
            LinkCorrection {
                expected_recording_id: 1,
                recording_id: 2,
            },
            1,
        )
        .await
        .unwrap();
        assert!(matches!(
            relink(
                &pool,
                LinkKind::File,
                1,
                LinkCorrection {
                    expected_recording_id: 1,
                    recording_id: 1
                },
                1
            )
            .await,
            Err(MetadataError::Conflict)
        ));
        let path: String = sqlx::query_scalar(
            "SELECT path FROM audio_files \
            WHERE id=1 AND recording_id=2",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(path, "/music/source.wav");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM metadata_link_revisions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
    }
}

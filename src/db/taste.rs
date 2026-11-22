//! Personal taste aggregation and bounded local recommendation generation.

use crate::db::discovery::DiscoveryError;
use crate::discovery::{
    ArtistAffinity, CandidateData, CandidateKind, Credit, Preferences,
    Profile, Suggestion, TrackSeed,
};
use sqlx::{SqlitePool, types::Json};
use std::collections::HashMap;

/// Persisted preference version and typed JSON arrays.
type PreferenceRow = (i64, Json<Vec<i64>>, Json<Vec<String>>);

/// Read explicit settings without writing an initial row.
pub(crate) async fn preferences(
    pool: &SqlitePool,
    user: i64,
) -> Result<Preferences, sqlx::Error> {
    let row: Option<PreferenceRow> = sqlx::query_as(
        "SELECT version,artist_ids,tags \
            FROM discovery_preferences WHERE user_id=?",
    )
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(row.map_or(
        Preferences {
            version: 0,
            artist_ids: vec![],
            tags: vec![],
        },
        |(version, Json(artist_ids), Json(tags))| Preferences {
            version,
            artist_ids,
            tags,
        },
    ))
}

/// Replace onboarding settings with an optimistic version and existing IDs.
pub(crate) async fn set_preferences(
    pool: &SqlitePool,
    user: i64,
    mut input: Preferences,
) -> Result<Preferences, DiscoveryError> {
    if input.version < 0
        || input.artist_ids.len() > 20
        || input.tags.len() > 20
    {
        return Err(DiscoveryError::Invalid("Invalid preference bounds"));
    }
    let mut ids = input.artist_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != input.artist_ids.len() || ids.iter().any(|id| *id <= 0) {
        return Err(DiscoveryError::Invalid(
            "Invalid or duplicate artist IDs",
        ));
    }
    let mut keys = std::collections::HashSet::new();
    for tag in &mut input.tags {
        *tag = crate::discovery::text(tag, 100)
            .map_err(DiscoveryError::Invalid)?;
        if !keys.insert(tag.to_lowercase()) {
            return Err(DiscoveryError::Invalid("Duplicate preference tags"));
        }
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let old: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT version,artist_ids,tags FROM discovery_preferences \
        WHERE user_id=?",
    )
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    let version = old.as_ref().map_or(0, |v| v.0);
    if version != input.version {
        return Err(DiscoveryError::Conflict);
    }
    let missing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 \
        FROM json_each(?) j LEFT JOIN artists a ON a.id=j.value \
        WHERE a.id IS NULL)",
    )
    .bind(Json(&input.artist_ids))
    .fetch_one(&mut *tx)
    .await?;
    if missing {
        return Err(DiscoveryError::Invalid("Unknown artist ID"));
    }
    let artist_json = serde_json::to_string(&input.artist_ids)?;
    let tag_json = serde_json::to_string(&input.tags)?;
    let changed = old
        .as_ref()
        .is_none_or(|v| v.1 != artist_json || v.2 != tag_json);
    if changed {
        sqlx::query(
            "INSERT INTO discovery_preferences \
            (user_id,version,artist_ids,tags) VALUES (?,1,?,?) \
            ON CONFLICT(user_id) DO UPDATE SET version=version+1, \
            artist_ids=excluded.artist_ids,tags=excluded.tags",
        )
        .bind(user)
        .bind(artist_json)
        .bind(tag_json)
        .execute(&mut *tx)
        .await?;
        input.version += 1;
    }
    tx.commit().await?;
    Ok(input)
}

/// One recording with source identities and a bounded personal weight.
#[derive(sqlx::FromRow)]
struct SeedRow {
    /// Current shared recording title.
    title: String,
    /// Ordered local credits as typed JSON.
    #[sqlx(json)]
    artists: Vec<Credit>,
    /// Sum of explicit like and decayed full-listen weights.
    weight: f64,
}

/// Aggregate only this account's signals; short plays never become dislikes.
pub(crate) async fn profile(
    pool: &SqlitePool,
    user: i64,
) -> Result<Profile, sqlx::Error> {
    let preferences = preferences(pool, user).await?;
    let rows: Vec<SeedRow> = sqlx::query_as(
        "WITH plays AS (SELECT recording_id, \
          MIN(8.0,SUM(CASE WHEN started_at>=unixepoch()-2592000 THEN 1.0 \
            WHEN started_at>=unixepoch()-7776000 THEN 0.5 ELSE 0.1 END)) w \
          FROM playback_events WHERE user_id=? AND kind='full' \
            AND started_at>=unixepoch()-31536000 GROUP BY recording_id), \
        seeds AS (SELECT r.id,r.title, \
          (CASE WHEN v.reaction='like' THEN 8.0 ELSE 0.0 END \
            +COALESCE(p.w,0.0)) weight FROM recordings r \
          LEFT JOIN plays p ON p.recording_id=r.id \
          LEFT JOIN recording_reactions v ON v.recording_id=r.id \
            AND v.user_id=? WHERE COALESCE(v.reaction,'')<>'dislike') \
        SELECT s.title,s.weight,COALESCE((SELECT json_group_array( \
            json_object('name',c.name,'mbid',c.mbid)) FROM ( \
          SELECT a.name,m.mbid FROM recording_artists ra \
          JOIN artists a ON a.id=ra.artist_id LEFT JOIN catalog_metadata m \
            ON m.entity_type='artist' AND m.entity_id=a.id \
          WHERE ra.recording_id=s.id ORDER BY ra.position) c),'[]') artists \
        FROM seeds s WHERE weight>0 ORDER BY weight DESC,id LIMIT 100",
    )
    .bind(user)
    .bind(user)
    .fetch_all(pool)
    .await?;
    let mut artists: HashMap<String, ArtistAffinity> = HashMap::new();
    let mut tracks = Vec::new();
    for row in rows {
        add_seed(
            &mut artists,
            &mut tracks,
            &row.title,
            &row.artists,
            row.weight,
        );
    }
    let external = crate::db::discovery::external_likes(pool, user).await?;
    for data in external {
        add_seed(&mut artists, &mut tracks, &data.title, &data.artists, 8.0);
    }
    let followed: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT a.name,m.mbid FROM artists a JOIN json_each(?) j \
        ON j.value=a.id LEFT JOIN catalog_metadata m \
        ON m.entity_type='artist' AND m.entity_id=a.id",
    )
    .bind(Json(&preferences.artist_ids))
    .fetch_all(pool)
    .await?;
    for (name, mbid) in followed {
        let key = mbid.clone().unwrap_or_else(|| name.to_lowercase());
        artists
            .entry(key)
            .and_modify(|a| a.weight = (a.weight + 12.0).min(64.0))
            .or_insert(ArtistAffinity {
                name,
                mbid,
                weight: 12.0,
            });
    }
    let mut artists: Vec<_> = artists.into_values().collect();
    artists.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    artists.truncate(20);
    tracks.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.artist.cmp(&b.artist))
            .then_with(|| a.title.cmp(&b.title))
    });
    let mut seen = std::collections::HashSet::new();
    tracks.retain(|track| {
        seen.insert((track.artist.to_lowercase(), track.title.to_lowercase()))
    });
    tracks.truncate(10);
    Ok(Profile {
        preferences,
        artists,
        tracks,
    })
}

/// Repeated observations have diminishing influence; seeds retain Unicode.
fn add_seed(
    artists: &mut HashMap<String, ArtistAffinity>,
    tracks: &mut Vec<TrackSeed>,
    title: &str,
    credits: &[Credit],
    weight: f64,
) {
    for credit in credits {
        let key = credit
            .mbid
            .clone()
            .unwrap_or_else(|| credit.name.to_lowercase());
        let contribution = weight.min(16.0) / (credits.len().max(1) as f64);
        artists
            .entry(key)
            .and_modify(|a| {
                a.weight = (a.weight + contribution).min(64.0);
                if a.mbid.is_none() {
                    a.mbid = credit.mbid.clone();
                }
            })
            .or_insert(ArtistAffinity {
                name: credit.name.clone(),
                mbid: credit.mbid.clone(),
                weight: contribution,
            });
    }
    if let Some(artist) = credits.first() {
        tracks.push(TrackSeed {
            title: title.into(),
            artist: artist.name.clone(),
            weight: weight.min(16.0),
        });
    }
}

/// Bounded candidate pool; SQL first favors related artists before rotation.
pub(crate) async fn local(
    pool: &SqlitePool,
    user: i64,
    profile: &Profile,
    rediscover: bool,
) -> Result<Vec<Suggestion>, sqlx::Error> {
    let rows: Vec<LocalRow> = sqlx::query_as(
        "SELECT r.id,r.title,m.mbid, \
        COALESCE((SELECT json_group_array(json_object( \
          'name',c.name,'mbid',c.mbid)) FROM (SELECT a.name,am.mbid \
          FROM recording_artists ra JOIN artists a ON a.id=ra.artist_id \
          LEFT JOIN catalog_metadata am ON am.entity_type='artist' \
            AND am.entity_id=a.id WHERE ra.recording_id=r.id \
          ORDER BY ra.position) c),'[]') artists \
        FROM recordings r LEFT JOIN catalog_metadata m \
          ON m.entity_type='recording' AND m.entity_id=r.id \
        LEFT JOIN recording_reactions v ON v.recording_id=r.id \
          AND v.user_id=? WHERE COALESCE(v.reaction,'')<>'dislike' \
          AND (?=0 OR (v.reaction='like' AND NOT EXISTS( \
            SELECT 1 FROM playback_events h WHERE h.user_id=? \
            AND h.recording_id=r.id AND h.kind='full' \
            AND h.started_at>=unixepoch()-2592000))) \
          AND (?=1 OR (COALESCE(v.reaction,'')<>'like' AND NOT EXISTS( \
            SELECT 1 FROM playback_events h WHERE h.user_id=? \
            AND h.recording_id=r.id AND h.kind='full' \
            AND h.started_at>=unixepoch()-604800))) \
        ORDER BY COALESCE((SELECT SUM(CAST(json_extract(j.value, \
          '$.weight') AS REAL)) FROM recording_artists ra \
          JOIN artists a ON a.id=ra.artist_id JOIN json_each(?) j \
          ON a.name=json_extract(j.value,'$.name') \
          WHERE ra.recording_id=r.id),0) DESC, \
          ((r.id*1103515245+?+unixepoch()/86400)%2147483647) LIMIT 1000",
    )
    .bind(user)
    .bind(rediscover)
    .bind(user)
    .bind(rediscover)
    .bind(user)
    .bind(Json(&profile.artists))
    .bind(user)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Suggestion {
            candidate: CandidateData {
                kind: CandidateKind::Recording,
                title: r.title,
                artists: r.artists,
                mbid: r.mbid,
                provider: "local".into(),
                external_id: format!("recording:{}", r.id),
                url: None,
                release_date: None,
            },
            weight: 1.0,
            reasons: vec![
                if rediscover {
                    "A favorite not played fully in 30 days"
                } else {
                    "From the shared library"
                }
                .into(),
            ],
        })
        .collect())
}

/// One bounded local catalog row with ordered display credits.
#[derive(sqlx::FromRow)]
struct LocalRow {
    /// Recording ID used only for a stable provider identity.
    id: i64,
    /// Current title.
    title: String,
    /// Confirmed external UUID if one exists.
    mbid: Option<String>,
    /// Ordered artist identities.
    #[sqlx(json)]
    artists: Vec<Credit>,
}

#[cfg(test)]
mod tests {
    //! Personal signals, onboarding, bounded repeats and offline generation.
    use super::*;
    use crate::db::discovery::tests::{generated, setup, suggestion};

    /// History belongs to its account and short plays never create negatives.
    #[tokio::test]
    async fn builds_profile_from_likes_follows_full_plays_and_external_likes()
    {
        let (_dir, pool) = setup().await;
        sqlx::query(
            "INSERT INTO recording_reactions \
            (user_id,recording_id,reaction,changed_at) \
            VALUES (1,1,'like',CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        for index in 0..20 {
            sqlx::query(
                "INSERT INTO playback_events \
                (user_id,event_id,recording_id,started_at,listened_ms,kind) \
                VALUES (1,?,2,unixepoch(),1000,'full')",
            )
            .bind(format!("repeat-{index}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO playback_events \
            (user_id,event_id,recording_id,started_at,listened_ms,kind) \
            VALUES (1,'preview',3,unixepoch(),100,'short')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let first = profile(&pool, 1).await.unwrap();
        assert_eq!(first.artists.len(), 1);
        assert_eq!(first.artists[0].name, "Кино");
        assert!(first.artists[0].weight <= 24.0);
        assert!(profile(&pool, 2).await.unwrap().artists.is_empty());
        let prefs = set_preferences(
            &pool,
            1,
            Preferences {
                version: 0,
                artist_ids: vec![2],
                tags: vec![" Русский рок ".into()],
            },
        )
        .await
        .unwrap();
        assert_eq!(prefs.tags, vec!["Русский рок"]);
        assert_eq!(prefs.version, 1);
        assert_eq!(
            set_preferences(&pool, 1, prefs.clone())
                .await
                .unwrap()
                .version,
            1
        );
        assert!(matches!(
            set_preferences(
                &pool,
                1,
                Preferences {
                    version: 0,
                    artist_ids: vec![],
                    tags: vec![]
                }
            )
            .await,
            Err(DiscoveryError::Conflict)
        ));
        let job = crate::db::discovery::admit(
            &pool,
            1,
            &crate::discovery::FeedRequest::Chart {},
            0,
        )
        .await
        .unwrap();
        let mut item = suggestion("lastfm", "external", "New track", None);
        item.candidate.artists[0].name = "New artist".into();
        crate::db::discovery::complete(&pool, &job, generated(vec![item]))
            .await
            .unwrap();
        let id = crate::db::discovery::contents(&pool, 1, job.feed_id)
            .await
            .unwrap()
            .1[0]
            .id;
        crate::db::discovery::feedback(
            &pool,
            1,
            id,
            crate::discovery::Feedback {
                version: 0,
                reaction: Some(crate::library::reactions::Reaction::Like),
                hidden: false,
                wanted: true,
            },
        )
        .await
        .unwrap();
        let after = profile(&pool, 1).await.unwrap();
        assert!(after.artists.iter().any(|a| a.name == "Other"));
        assert!(after.tracks.iter().any(|a| a.artist == "New artist"));
        let candidates = local(&pool, 1, &after, false).await.unwrap();
        assert_eq!(candidates.len(), 1);
        pool.close().await;
    }

    /// Rediscovery excludes recently played favorites without reading files.
    #[tokio::test]
    async fn rediscovery_uses_full_listens_and_rejects_invalid_onboarding() {
        let (_dir, pool) = setup().await;
        sqlx::query(
            "INSERT INTO recording_reactions \
            (user_id,recording_id,reaction,changed_at) VALUES \
            (1,1,'like',CURRENT_TIMESTAMP),(1,2,'like',CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO playback_events \
            (user_id,event_id,recording_id,started_at,listened_ms,kind) \
            VALUES (1,'recent',2,unixepoch(),1000,'full')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let profile = profile(&pool, 1).await.unwrap();
        let items = local(&pool, 1, &profile, true).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].candidate.external_id, "recording:1");
        for input in [
            Preferences {
                version: 0,
                artist_ids: vec![999],
                tags: vec![],
            },
            Preferences {
                version: 0,
                artist_ids: vec![1, 1],
                tags: vec![],
            },
            Preferences {
                version: 0,
                artist_ids: vec![],
                tags: vec!["РОК".into(), "рок".into()],
            },
        ] {
            assert!(matches!(
                set_preferences(&pool, 1, input).await,
                Err(DiscoveryError::Invalid(_))
            ));
        }
        assert_eq!(preferences(&pool, 1).await.unwrap().version, 0);
        pool.close().await;
    }
}

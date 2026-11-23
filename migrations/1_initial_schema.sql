-- Initial SQLite schema. Applied atomically by SQLx before startup.

-- Shared music catalog
-- --------------------

-- Names are deliberately not unique: distinct artists may share a name.
CREATE TABLE artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255)
) STRICT;

CREATE TABLE releases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 255),
    kind TEXT NOT NULL CHECK (
        kind IN ('album', 'single', 'ep', 'compilation', 'other')
    )
) STRICT;

-- Store credit order separately from artist identity.
CREATE TABLE release_artists (
    release_id INTEGER NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    artist_id INTEGER NOT NULL REFERENCES artists(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (release_id, artist_id),
    UNIQUE (release_id, position)
) STRICT;

CREATE INDEX release_artists_artist_id ON release_artists(artist_id);

-- Recording identity is independent of releases and physical audio files.
CREATE TABLE recordings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 255)
) STRICT;

CREATE TABLE recording_artists (
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    artist_id INTEGER NOT NULL REFERENCES artists(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (recording_id, artist_id),
    UNIQUE (recording_id, position)
) STRICT;

CREATE INDEX recording_artists_artist_id ON recording_artists(artist_id);

-- The same recording may occupy different releases or track positions.
CREATE TABLE release_tracks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    release_id INTEGER NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    disc_number INTEGER NOT NULL CHECK (disc_number > 0),
    position INTEGER NOT NULL CHECK (position > 0),
    UNIQUE (release_id, disc_number, position)
) STRICT;

CREATE INDEX release_tracks_recording_id ON release_tracks(recording_id);

-- Properties are declared metadata until an audio probe verifies them.
CREATE TABLE audio_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    path TEXT NOT NULL UNIQUE CHECK (
        substr(path, 1, 1) = '/' AND
        length(CAST(path AS BLOB)) BETWEEN 1 AND 4096 AND
        instr(path, char(0)) = 0
    ),
    format TEXT NOT NULL CHECK (length(format) BETWEEN 1 AND 32),
    size_bytes INTEGER CHECK (size_bytes > 0),
    duration_ms INTEGER CHECK (duration_ms > 0),
    sample_rate_hz INTEGER CHECK (sample_rate_hz > 0),
    channels INTEGER CHECK (channels > 0),
    bit_depth INTEGER CHECK (bit_depth > 0),
    bitrate_bps INTEGER CHECK (bitrate_bps > 0)
) STRICT;

CREATE INDEX audio_files_recording_id ON audio_files(recording_id);

-- Library scans and file observations
-- -----------------------------------

-- Persist observations separately from curated catalog metadata.
CREATE TABLE audio_file_tags (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    tags TEXT NOT NULL CHECK (json_valid(tags))
) STRICT;

CREATE TABLE library_scans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    status TEXT NOT NULL CHECK (
        status IN ('running', 'completed', 'failed', 'interrupted')
    ),
    roots TEXT NOT NULL CHECK (json_valid(roots)),
    imported INTEGER NOT NULL DEFAULT 0 CHECK (imported >= 0),
    refreshed INTEGER NOT NULL DEFAULT 0 CHECK (refreshed >= 0),
    skipped INTEGER NOT NULL DEFAULT 0 CHECK (skipped >= 0),
    errors INTEGER NOT NULL DEFAULT 0 CHECK (errors >= 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    message TEXT,
    unchanged INTEGER NOT NULL DEFAULT 0 CHECK (unchanged >= 0),
    force INTEGER NOT NULL DEFAULT 0 CHECK (force IN (0, 1))
) STRICT;

-- Enforce one active scan even when HTTP requests race.
CREATE UNIQUE INDEX library_scans_active ON library_scans((1))
    WHERE status = 'running';

CREATE TABLE library_scan_errors (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_id INTEGER NOT NULL REFERENCES library_scans(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    message TEXT NOT NULL
) STRICT;

CREATE INDEX library_scan_errors_scan_id
    ON library_scan_errors(scan_id, id);

-- Scanner-local keys never identify manual entities by their display names.
-- Keep source keys stable if catalog display names are corrected later.
CREATE TABLE scan_artist_matches (
    directory TEXT NOT NULL,
    source_name TEXT NOT NULL,
    artist_id INTEGER NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    PRIMARY KEY (directory, source_name)
) STRICT;

CREATE INDEX scan_artist_matches_artist_id
    ON scan_artist_matches(artist_id);

CREATE TABLE scan_release_matches (
    directory TEXT NOT NULL,
    source_title TEXT NOT NULL,
    album_credit TEXT NOT NULL,
    release_id INTEGER NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    PRIMARY KEY (directory, source_title, album_credit)
) STRICT;

CREATE INDEX scan_release_matches_release_id
    ON scan_release_matches(release_id);

-- Only successfully probed and committed files receive a reusable snapshot.
CREATE TABLE audio_file_states (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (json_valid(state))
) STRICT;

CREATE TABLE audio_file_hashes (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL CHECK (
        length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'
    )
) STRICT;

CREATE INDEX audio_file_hashes_sha256 ON audio_file_hashes(sha256);

-- Only new scanner-created recordings claim an explicit source identity.
CREATE TABLE scan_recording_matches (
    mbid TEXT PRIMARY KEY CHECK (length(mbid) = 36),
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE
) STRICT;

CREATE INDEX scan_recording_matches_recording_id
    ON scan_recording_matches(recording_id);

-- Accounts, credentials and recording reactions
-- ---------------------------------------------

CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64),
    role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'admin'))
) STRICT;
-- Raw bearer credentials never reach storage.
CREATE TABLE user_keys (
    digest TEXT PRIMARY KEY CHECK (length(digest) = 64),
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE
) STRICT;
CREATE INDEX user_keys_user_id ON user_keys(user_id);
CREATE TABLE recording_reactions (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    reaction TEXT NOT NULL CHECK (reaction IN ('like', 'dislike')),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (user_id, recording_id)
) STRICT;
CREATE INDEX recording_reactions_favorites
    ON recording_reactions(
        user_id, reaction, changed_at DESC, recording_id DESC
    );

-- Passwords are optional and stored separately from account identities.
CREATE TABLE user_passwords (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    hash TEXT NOT NULL
) STRICT;

-- Session credentials are independent of long-lived personal keys.
CREATE TABLE user_sessions (
    digest TEXT PRIMARY KEY CHECK (length(digest) = 64),
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX user_sessions_user_id ON user_sessions(user_id);
CREATE INDEX user_sessions_expiry ON user_sessions(expires_at);

-- Playback history
-- ----------------

-- One immutable client report per actual playback, independent of HTTP ranges.
CREATE TABLE playback_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL CHECK (length(event_id) BETWEEN 1 AND 128),
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    listened_ms INTEGER NOT NULL CHECK (listened_ms > 0),
    duration_ms INTEGER CHECK (duration_ms > 0),
    kind TEXT NOT NULL CHECK (kind IN ('full', 'short')),
    received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (user_id, event_id),
    CHECK (duration_ms IS NULL OR listened_ms <= duration_ms)
) STRICT;
CREATE INDEX playback_events_history
    ON playback_events(user_id, started_at DESC, id DESC);

-- Playlists and playback queues
-- -----------------------------

CREATE TABLE playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    is_public INTEGER NOT NULL DEFAULT 0 CHECK (is_public IN (0, 1))
) STRICT;
CREATE INDEX playlists_user_id ON playlists(user_id, id);

-- Position identifies an occurrence, so a recording may appear several times.
CREATE TABLE playlist_tracks (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position > 0),
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    PRIMARY KEY (playlist_id, position)
) STRICT;
CREATE INDEX playlist_tracks_recording_id ON playlist_tracks(recording_id);

-- Publication is explicit and limited to authenticated instance members.
CREATE INDEX playlists_published ON playlists(id) WHERE is_public = 1;

-- One persistent playback snapshot per account; absent means empty version 0.
CREATE TABLE playback_queues (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    current_position INTEGER CHECK (current_position > 0),
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    CHECK (current_position IS NOT NULL OR position_ms = 0)
) STRICT;
CREATE TABLE playback_queue_tracks (
    user_id INTEGER NOT NULL REFERENCES playback_queues(user_id)
        ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position > 0),
    recording_id INTEGER NOT NULL REFERENCES recordings(id)
        ON DELETE RESTRICT,
    PRIMARY KEY (user_id, position)
) STRICT;
CREATE INDEX playback_queue_recording
    ON playback_queue_tracks(recording_id);

-- Catalog metadata and artwork
-- ----------------------------

-- Entity registry supports one version across edits, identities and artwork.
CREATE TABLE catalog_metadata (
    entity_type TEXT NOT NULL CHECK (
        entity_type IN ('artist', 'release', 'recording')
    ),
    entity_id INTEGER NOT NULL,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    mbid TEXT CHECK (length(mbid) = 36),
    source TEXT NOT NULL DEFAULT 'existing',
    PRIMARY KEY (entity_type, entity_id),
    UNIQUE (entity_type, mbid)
) STRICT;
CREATE TRIGGER metadata_artist_insert AFTER INSERT ON artists BEGIN
    INSERT INTO catalog_metadata (entity_type,entity_id)
        VALUES ('artist',new.id);
END;
CREATE TRIGGER metadata_release_insert AFTER INSERT ON releases BEGIN
    INSERT INTO catalog_metadata (entity_type,entity_id)
        VALUES ('release',new.id);
END;
CREATE TRIGGER metadata_recording_insert AFTER INSERT ON recordings BEGIN
    INSERT INTO catalog_metadata (entity_type,entity_id)
        VALUES ('recording',new.id);
END;
CREATE TRIGGER metadata_artist_delete AFTER DELETE ON artists BEGIN
    DELETE FROM catalog_metadata
        WHERE entity_type = 'artist' AND entity_id = old.id;
END;
CREATE TRIGGER metadata_release_delete AFTER DELETE ON releases BEGIN
    DELETE FROM catalog_metadata
        WHERE entity_type = 'release' AND entity_id = old.id;
END;
CREATE TRIGGER metadata_recording_delete AFTER DELETE ON recordings BEGIN
    DELETE FROM catalog_metadata
        WHERE entity_type = 'recording' AND entity_id = old.id;
END;
CREATE TABLE catalog_artwork (
    entity_type TEXT NOT NULL,
    entity_id INTEGER NOT NULL,
    mime TEXT NOT NULL CHECK (mime IN ('image/png','image/jpeg')),
    digest TEXT NOT NULL CHECK (length(digest) = 64),
    data BLOB NOT NULL CHECK (length(data) BETWEEN 1 AND 5242880),
    source TEXT NOT NULL,
    PRIMARY KEY (entity_type,entity_id),
    FOREIGN KEY (entity_type,entity_id)
        REFERENCES catalog_metadata(entity_type,entity_id) ON DELETE CASCADE
) STRICT;
CREATE TABLE metadata_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type TEXT NOT NULL,
    entity_id INTEGER NOT NULL,
    action TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running','completed','failed','interrupted')),
    message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    FOREIGN KEY (entity_type,entity_id)
        REFERENCES catalog_metadata(entity_type,entity_id) ON DELETE CASCADE
) STRICT;
CREATE UNIQUE INDEX metadata_one_running ON metadata_jobs(status)
    WHERE status = 'running';
CREATE TABLE metadata_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id INTEGER NOT NULL REFERENCES metadata_jobs(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','applied','rejected')),
    proposal TEXT NOT NULL CHECK (json_valid(proposal)),
    artwork BLOB CHECK (length(artwork) BETWEEN 1 AND 5242880)
) STRICT;
CREATE INDEX metadata_candidates_job ON metadata_candidates(job_id,id);
CREATE TABLE metadata_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type TEXT NOT NULL,
    entity_id INTEGER NOT NULL,
    version INTEGER NOT NULL,
    user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    source TEXT NOT NULL,
    snapshot TEXT NOT NULL CHECK (json_valid(snapshot)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (entity_type,entity_id)
        REFERENCES catalog_metadata(entity_type,entity_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX metadata_revisions_entity
    ON metadata_revisions(entity_type,entity_id,id);
CREATE TRIGGER metadata_scanner_identity AFTER INSERT ON scan_recording_matches
BEGIN
    UPDATE catalog_metadata SET mbid=new.mbid
    WHERE entity_type='recording' AND entity_id=new.recording_id
        AND mbid IS NULL AND source='existing';
END;
-- Explicit association corrections do not merge personal recording data.
CREATE TABLE metadata_link_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    link_type TEXT NOT NULL CHECK (link_type IN ('file','release_track')),
    link_id INTEGER NOT NULL,
    old_recording_id INTEGER NOT NULL,
    new_recording_id INTEGER NOT NULL,
    user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

-- Discovery feeds and preferences
-- -------------------------------

-- Discovery never materializes an unselected external result in the catalog.
CREATE TABLE discovery_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('recording','release','playlist')),
    mbid TEXT,
    document TEXT NOT NULL CHECK (json_valid(document))
) STRICT;
CREATE UNIQUE INDEX discovery_candidate_mbid
    ON discovery_candidates(kind,mbid) WHERE mbid IS NOT NULL;
CREATE TABLE discovery_origins (
    provider TEXT NOT NULL,
    external_id TEXT NOT NULL,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id)
        ON DELETE CASCADE,
    PRIMARY KEY (provider,external_id)
) STRICT;
CREATE INDEX discovery_origins_candidate ON discovery_origins(candidate_id);
CREATE TABLE discovery_preferences (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    artist_ids TEXT NOT NULL CHECK (json_valid(artist_ids)),
    tags TEXT NOT NULL CHECK (json_valid(tags))
) STRICT;
CREATE TABLE discovery_feeds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    request TEXT NOT NULL CHECK (json_valid(request)),
    version INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    source_url TEXT,
    generated_at INTEGER,
    warnings TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(warnings)),
    UNIQUE (user_id,request_key)
) STRICT;
CREATE TABLE discovery_feed_items (
    feed_id INTEGER NOT NULL REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id),
    position INTEGER NOT NULL,
    weight REAL NOT NULL,
    reasons TEXT NOT NULL CHECK (json_valid(reasons)),
    PRIMARY KEY (feed_id,candidate_id),
    UNIQUE (feed_id,position)
) STRICT;
CREATE TABLE discovery_feedback (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id),
    version INTEGER NOT NULL CHECK (version > 0),
    reaction TEXT CHECK (reaction IN ('like','dislike')),
    hidden INTEGER NOT NULL CHECK (hidden IN (0,1)),
    wanted INTEGER NOT NULL CHECK (wanted IN (0,1)),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (user_id,candidate_id)
) STRICT;
CREATE INDEX discovery_wishlist ON discovery_feedback(user_id,wanted);
CREATE TABLE discovery_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    feed_id INTEGER NOT NULL REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'running' CHECK (
        status IN ('running','completed','failed','interrupted')
    ),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    finished_at INTEGER,
    message TEXT
) STRICT;
CREATE UNIQUE INDEX discovery_one_running ON discovery_jobs(status)
    WHERE status='running';
CREATE INDEX discovery_job_owner ON discovery_jobs(user_id,id DESC);
-- Responses are shared only within the same operator-configured instance.
CREATE TABLE discovery_provider_cache (
    provider TEXT NOT NULL,
    request_key TEXT NOT NULL,
    document TEXT NOT NULL CHECK (json_valid(document)),
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (provider,request_key)
) STRICT;

-- Downloads, imports and reaction decisions
-- -----------------------------------------

-- Experimental provider jobs; only validated files enter the shared catalog.
CREATE TABLE download_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    operation TEXT NOT NULL CHECK (operation IN ('search', 'download')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
        'queued', 'running', 'verifying', 'importing', 'completed',
        'failed', 'cancelled', 'interrupted'
    )),
    target TEXT NOT NULL CHECK (json_valid(target)),
    target_key TEXT,
    video_id TEXT,
    retry_of INTEGER REFERENCES download_jobs(id) ON DELETE SET NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0
        CHECK (cancel_requested IN (0, 1)),
    downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (downloaded_bytes >= 0),
    total_bytes INTEGER CHECK (total_bytes > 0),
    recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
    file_id INTEGER REFERENCES audio_files(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    finished_at INTEGER,
    message TEXT
) STRICT;
CREATE INDEX download_jobs_owner ON download_jobs(user_id, id DESC);
CREATE UNIQUE INDEX download_jobs_active_source ON download_jobs(video_id)
    WHERE operation = 'download'
    AND status IN ('queued', 'running', 'verifying', 'importing');
CREATE UNIQUE INDEX download_jobs_active_target ON download_jobs(target_key)
    WHERE operation = 'download'
    AND status IN ('queued', 'running', 'verifying', 'importing');

CREATE TABLE download_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id INTEGER NOT NULL REFERENCES download_jobs(id) ON DELETE CASCADE,
    video_id TEXT NOT NULL CHECK (length(video_id) = 11),
    title TEXT NOT NULL,
    channel TEXT,
    duration_seconds INTEGER,
    UNIQUE(job_id, video_id)
) STRICT;
-- Stable completed provenance also prevents fetching the same video again.
CREATE TABLE downloaded_sources (
    video_id TEXT PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES audio_files(id) ON DELETE CASCADE
) STRICT;
-- Journal intent BEFORE publishing a file; recovery finalizes interrupted SQL.
CREATE TABLE download_imports (
    job_id INTEGER PRIMARY KEY REFERENCES download_jobs(id) ON DELETE RESTRICT,
    sha256 TEXT NOT NULL,
    path TEXT NOT NULL
) STRICT;
CREATE TABLE discovery_download_links (
    candidate_id INTEGER PRIMARY KEY
        REFERENCES discovery_candidates(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE
) STRICT;
-- Remember explicit clears too, so delayed imports cannot restore old likes.
CREATE TABLE recording_reaction_decisions (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    changed_at TEXT NOT NULL,
    PRIMARY KEY(user_id, recording_id)
) STRICT;
CREATE TRIGGER reaction_decision_insert AFTER INSERT ON recording_reactions
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        NEW.user_id, NEW.recording_id, NEW.changed_at
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;
CREATE TRIGGER reaction_decision_update AFTER UPDATE ON recording_reactions
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        NEW.user_id, NEW.recording_id, NEW.changed_at
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;
CREATE TRIGGER reaction_decision_delete AFTER DELETE ON recording_reactions
WHEN EXISTS(SELECT 1 FROM users WHERE id = OLD.user_id)
    AND EXISTS(SELECT 1 FROM recordings WHERE id = OLD.recording_id)
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        OLD.user_id, OLD.recording_id,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;

-- Automation schedules
-- --------------------

-- One scheduler reuses existing supervised workers. All schedules are opt-in.
CREATE TABLE automation_schedules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('library','discovery','metadata')),
    owner_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    feed_id INTEGER REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    entity_type TEXT,
    entity_id INTEGER,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    interval_seconds INTEGER NOT NULL
        CHECK (interval_seconds BETWEEN 1 AND 2592000),
    next_run_at INTEGER,
    last_run_at INTEGER,
    last_status TEXT NOT NULL DEFAULT 'idle' CHECK (last_status IN (
        'idle','dispatching','running','completed','failed','deferred','paused'
    )),
    last_job_id INTEGER,
    failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    message TEXT,
    FOREIGN KEY (entity_type,entity_id)
        REFERENCES catalog_metadata(entity_type,entity_id) ON DELETE CASCADE,
    CHECK (
        (kind='library' AND owner_id IS NULL AND feed_id IS NULL
            AND entity_type IS NULL AND entity_id IS NULL) OR
        (kind='discovery' AND owner_id IS NOT NULL AND feed_id IS NOT NULL
            AND entity_type IS NULL AND entity_id IS NULL) OR
        (kind='metadata' AND owner_id IS NOT NULL AND feed_id IS NULL
            AND entity_type IS NOT NULL AND entity_id IS NOT NULL)
    )
) STRICT;
CREATE UNIQUE INDEX automation_library ON automation_schedules(kind)
    WHERE kind='library';
CREATE UNIQUE INDEX automation_feed ON automation_schedules(feed_id)
    WHERE kind='discovery';
CREATE UNIQUE INDEX automation_metadata
    ON automation_schedules(entity_type,entity_id) WHERE kind='metadata';
CREATE INDEX automation_due ON automation_schedules(next_run_at,id)
    WHERE enabled=1;

-- Media access, file removal and client read models
-- -------------------------------------------------

-- Scoped reusable media credentials, never accepted by the general API.
CREATE TABLE media_tickets (
    digest TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_digest TEXT NOT NULL,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES audio_files(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX media_tickets_expiry ON media_tickets(expires_at);
CREATE INDEX media_tickets_owner ON media_tickets(user_id);

-- Intent survives unlink/SQL crashes. File identity stays available for audit.
CREATE TABLE file_removals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recording_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    sha256 TEXT,
    remove_file INTEGER NOT NULL CHECK (remove_file IN (0,1)),
    requested_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','completed','failed')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    finished_at INTEGER,
    message TEXT
) STRICT;
CREATE UNIQUE INDEX file_removal_one_pending ON file_removals(status)
    WHERE status='pending';
CREATE INDEX file_removals_file ON file_removals(file_id,status);
CREATE TRIGGER file_removal_preserve_identity
    BEFORE UPDATE OF path,recording_id ON audio_files
    WHEN EXISTS(SELECT 1 FROM file_removals d
        WHERE d.file_id=OLD.id AND d.status='pending')
BEGIN
    SELECT RAISE(ABORT,'audio_file_removal_pending');
END;

-- Rich read models avoid one HTTP request per credit/file in client lists.
CREATE VIEW client_recordings AS
WITH data AS (
    SELECT r.id,r.title,
        (SELECT json_group_array(artist_id) FROM (
            SELECT artist_id FROM recording_artists
            WHERE recording_id=r.id ORDER BY position
        )) artist_ids,
        (SELECT json_group_array(json_object('id',id,'name',name)) FROM (
            SELECT a.id,a.name FROM recording_artists ra
            JOIN artists a ON a.id=ra.artist_id
            WHERE ra.recording_id=r.id ORDER BY ra.position
        )) artists,
        (SELECT json_group_array(json_object(
            'id',f.id,'format',f.format,'size_bytes',f.size_bytes,
            'duration_ms',f.duration_ms,'sample_rate_hz',f.sample_rate_hz,
            'channels',f.channels,'bit_depth',f.bit_depth,
            'bitrate_bps',f.bitrate_bps,
            'stream_url','/api/v1/recordings/'||r.id||'/files/'
                ||f.id||'/stream'
        )) FROM (
            SELECT f.* FROM audio_files f WHERE f.recording_id=r.id
                AND NOT EXISTS(SELECT 1 FROM file_removals d
                    WHERE d.file_id=f.id AND d.status='pending')
            ORDER BY f.id
        ) f) files,
        (SELECT count(*) FROM audio_files f WHERE f.recording_id=r.id
            AND NOT EXISTS(SELECT 1 FROM file_removals d
                WHERE d.file_id=f.id AND d.status='pending')) audio_file_count,
        (SELECT f.duration_ms FROM audio_files f WHERE f.recording_id=r.id
            AND f.duration_ms IS NOT NULL
            AND NOT EXISTS(SELECT 1 FROM file_removals d
                WHERE d.file_id=f.id AND d.status='pending')
            ORDER BY f.id LIMIT 1) duration_ms,
        COALESCE(
            (SELECT '/api/v1/artwork/recording/'||r.id
                FROM catalog_artwork a
                WHERE a.entity_type='recording' AND a.entity_id=r.id),
            (SELECT '/api/v1/artwork/release/'||t.release_id
                FROM release_tracks t JOIN catalog_artwork a
                ON a.entity_type='release' AND a.entity_id=t.release_id
                WHERE t.recording_id=r.id ORDER BY t.release_id LIMIT 1)
        ) artwork_url
    FROM recordings r
)
SELECT *,audio_file_count>0 has_audio FROM data;

CREATE VIEW client_releases AS
WITH data AS (
    SELECT r.id,r.title,r.kind,
        (SELECT json_group_array(artist_id) FROM (
            SELECT artist_id FROM release_artists
            WHERE release_id=r.id ORDER BY position
        )) artist_ids,
        (SELECT json_group_array(json_object('id',id,'name',name)) FROM (
            SELECT a.id,a.name FROM release_artists ra
            JOIN artists a ON a.id=ra.artist_id
            WHERE ra.release_id=r.id ORDER BY ra.position
        )) artists,
        (SELECT count(*) FROM release_tracks t WHERE t.release_id=r.id)
            track_count,
        (SELECT count(*) FROM release_tracks t
            JOIN client_recordings c ON c.id=t.recording_id
            WHERE t.release_id=r.id AND c.has_audio) available_track_count,
        (SELECT CASE WHEN count(*)>0 AND count(c.duration_ms)=count(*)
            THEN sum(c.duration_ms) ELSE NULL END
            FROM release_tracks t
            JOIN client_recordings c ON c.id=t.recording_id
            WHERE t.release_id=r.id) duration_ms,
        (SELECT '/api/v1/artwork/release/'||r.id FROM catalog_artwork a
            WHERE a.entity_type='release' AND a.entity_id=r.id) artwork_url
    FROM releases r
)
SELECT *,available_track_count>0 has_audio,
    track_count>0 AND available_track_count=track_count is_complete FROM data;

CREATE VIEW client_artists AS
SELECT a.id,a.name,
    (SELECT count(*) FROM recording_artists r WHERE r.artist_id=a.id)
        recording_count,
    (SELECT count(*) FROM release_artists r WHERE r.artist_id=a.id)
        release_count,
    (SELECT '/api/v1/artwork/artist/'||a.id FROM catalog_artwork c
        WHERE c.entity_type='artist' AND c.entity_id=a.id) artwork_url
FROM artists a;

-- Full-text catalog search
-- ------------------------

-- Unicode token-prefix search over the shared catalog, not physical files.
CREATE VIRTUAL TABLE catalog_search USING fts5(
    kind UNINDEXED, entity_id UNINDEXED, text,
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TRIGGER artists_search_insert AFTER INSERT ON artists BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('artist', new.id, new.name);
END;
CREATE TRIGGER artists_search_delete AFTER DELETE ON artists BEGIN
    DELETE FROM catalog_search WHERE kind = 'artist' AND entity_id = old.id;
END;
CREATE TRIGGER artists_search_update AFTER UPDATE OF id, name
ON artists BEGIN
    DELETE FROM catalog_search WHERE kind = 'artist' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('artist', new.id, new.name);
END;

CREATE TRIGGER releases_search_insert AFTER INSERT ON releases BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('release', new.id, new.title);
END;
CREATE TRIGGER releases_search_delete AFTER DELETE ON releases BEGIN
    DELETE FROM catalog_search WHERE kind = 'release' AND entity_id = old.id;
END;
CREATE TRIGGER releases_search_update AFTER UPDATE OF id, title
ON releases BEGIN
    DELETE FROM catalog_search WHERE kind = 'release' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('release', new.id, new.title);
END;

CREATE TRIGGER recordings_search_insert AFTER INSERT ON recordings BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('recording', new.id, new.title);
END;
CREATE TRIGGER recordings_search_delete AFTER DELETE ON recordings BEGIN
    DELETE FROM catalog_search WHERE kind = 'recording' AND entity_id = old.id;
END;
CREATE TRIGGER recordings_search_update AFTER UPDATE OF id, title
ON recordings BEGIN
    DELETE FROM catalog_search WHERE kind = 'recording' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('recording', new.id, new.title);
END;

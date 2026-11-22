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

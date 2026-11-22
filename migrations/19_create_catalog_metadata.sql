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
INSERT INTO catalog_metadata (entity_type, entity_id)
    SELECT 'artist', id FROM artists;
INSERT INTO catalog_metadata (entity_type, entity_id)
    SELECT 'release', id FROM releases;
INSERT INTO catalog_metadata (entity_type, entity_id)
    SELECT 'recording', id FROM recordings;
-- Existing scanner identities are retained as provenance, not silently merged.
UPDATE catalog_metadata SET mbid = (
    SELECT mbid FROM scan_recording_matches
    WHERE recording_id = entity_id ORDER BY mbid LIMIT 1
) WHERE entity_type = 'recording';
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

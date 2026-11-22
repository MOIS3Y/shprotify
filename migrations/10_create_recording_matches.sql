-- Only new scanner-created recordings claim an explicit source identity.
CREATE TABLE scan_recording_matches (
    mbid TEXT PRIMARY KEY CHECK (length(mbid) = 36),
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE
) STRICT;

CREATE INDEX scan_recording_matches_recording_id
    ON scan_recording_matches(recording_id);

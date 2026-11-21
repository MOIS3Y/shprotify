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

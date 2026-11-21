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

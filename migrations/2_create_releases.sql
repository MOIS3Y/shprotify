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

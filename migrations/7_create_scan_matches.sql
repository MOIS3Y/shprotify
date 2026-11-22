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

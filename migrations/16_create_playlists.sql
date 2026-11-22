CREATE TABLE playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    description TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
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

-- Publication is explicit and limited to authenticated instance members.
ALTER TABLE playlists ADD COLUMN is_public INTEGER NOT NULL DEFAULT 0
    CHECK (is_public IN (0, 1));
CREATE INDEX playlists_published ON playlists(id) WHERE is_public = 1;

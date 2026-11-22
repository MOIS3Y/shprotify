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

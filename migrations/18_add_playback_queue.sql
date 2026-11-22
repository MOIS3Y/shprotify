-- One persistent playback snapshot per account; absent means empty version 0.
CREATE TABLE playback_queues (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    current_position INTEGER CHECK (current_position > 0),
    position_ms INTEGER NOT NULL CHECK (position_ms >= 0),
    CHECK (current_position IS NOT NULL OR position_ms = 0)
) STRICT;
CREATE TABLE playback_queue_tracks (
    user_id INTEGER NOT NULL REFERENCES playback_queues(user_id)
        ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position > 0),
    recording_id INTEGER NOT NULL REFERENCES recordings(id)
        ON DELETE RESTRICT,
    PRIMARY KEY (user_id, position)
) STRICT;
CREATE INDEX playback_queue_recording
    ON playback_queue_tracks(recording_id);

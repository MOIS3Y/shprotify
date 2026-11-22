CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64)
) STRICT;
-- Raw bearer credentials never reach storage.
CREATE TABLE user_keys (
    digest TEXT PRIMARY KEY CHECK (length(digest) = 64),
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE
) STRICT;
CREATE INDEX user_keys_user_id ON user_keys(user_id);
CREATE TABLE recording_reactions (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    reaction TEXT NOT NULL CHECK (reaction IN ('like', 'dislike')),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (user_id, recording_id)
) STRICT;
CREATE INDEX recording_reactions_favorites
    ON recording_reactions(user_id, reaction, changed_at DESC, recording_id DESC);

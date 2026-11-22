-- Discovery never materializes an unselected external result in the catalog.
CREATE TABLE discovery_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('recording','release','playlist')),
    mbid TEXT,
    document TEXT NOT NULL CHECK (json_valid(document))
) STRICT;
CREATE UNIQUE INDEX discovery_candidate_mbid
    ON discovery_candidates(kind,mbid) WHERE mbid IS NOT NULL;
CREATE TABLE discovery_origins (
    provider TEXT NOT NULL,
    external_id TEXT NOT NULL,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id)
        ON DELETE CASCADE,
    PRIMARY KEY (provider,external_id)
) STRICT;
CREATE INDEX discovery_origins_candidate ON discovery_origins(candidate_id);
CREATE TABLE discovery_preferences (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    artist_ids TEXT NOT NULL CHECK (json_valid(artist_ids)),
    tags TEXT NOT NULL CHECK (json_valid(tags))
) STRICT;
CREATE TABLE discovery_feeds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    request_key TEXT NOT NULL,
    request TEXT NOT NULL CHECK (json_valid(request)),
    version INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    source_url TEXT,
    generated_at INTEGER,
    warnings TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(warnings)),
    UNIQUE (user_id,request_key)
) STRICT;
CREATE TABLE discovery_feed_items (
    feed_id INTEGER NOT NULL REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id),
    position INTEGER NOT NULL,
    weight REAL NOT NULL,
    reasons TEXT NOT NULL CHECK (json_valid(reasons)),
    PRIMARY KEY (feed_id,candidate_id),
    UNIQUE (feed_id,position)
) STRICT;
CREATE TABLE discovery_feedback (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    candidate_id INTEGER NOT NULL REFERENCES discovery_candidates(id),
    version INTEGER NOT NULL CHECK (version > 0),
    reaction TEXT CHECK (reaction IN ('like','dislike')),
    hidden INTEGER NOT NULL CHECK (hidden IN (0,1)),
    wanted INTEGER NOT NULL CHECK (wanted IN (0,1)),
    changed_at TEXT NOT NULL,
    PRIMARY KEY (user_id,candidate_id)
) STRICT;
CREATE INDEX discovery_wishlist ON discovery_feedback(user_id,wanted);
CREATE TABLE discovery_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    feed_id INTEGER NOT NULL REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'running' CHECK (
        status IN ('running','completed','failed','interrupted')
    ),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    finished_at INTEGER,
    message TEXT
) STRICT;
CREATE UNIQUE INDEX discovery_one_running ON discovery_jobs(status)
    WHERE status='running';
CREATE INDEX discovery_job_owner ON discovery_jobs(user_id,id DESC);
-- Responses are shared only within the same operator-configured instance.
CREATE TABLE discovery_provider_cache (
    provider TEXT NOT NULL,
    request_key TEXT NOT NULL,
    document TEXT NOT NULL CHECK (json_valid(document)),
    fetched_at INTEGER NOT NULL,
    PRIMARY KEY (provider,request_key)
) STRICT;

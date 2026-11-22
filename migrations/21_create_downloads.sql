-- Experimental provider jobs; only validated files enter the shared catalog.
CREATE TABLE download_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    operation TEXT NOT NULL CHECK (operation IN ('search', 'download')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
        'queued', 'running', 'verifying', 'importing', 'completed',
        'failed', 'cancelled', 'interrupted'
    )),
    target TEXT NOT NULL CHECK (json_valid(target)),
    target_key TEXT,
    video_id TEXT,
    retry_of INTEGER REFERENCES download_jobs(id) ON DELETE SET NULL,
    cancel_requested INTEGER NOT NULL DEFAULT 0
        CHECK (cancel_requested IN (0, 1)),
    downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (downloaded_bytes >= 0),
    total_bytes INTEGER CHECK (total_bytes > 0),
    recording_id INTEGER REFERENCES recordings(id) ON DELETE SET NULL,
    file_id INTEGER REFERENCES audio_files(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    finished_at INTEGER,
    message TEXT
) STRICT;
CREATE INDEX download_jobs_owner ON download_jobs(user_id, id DESC);
CREATE UNIQUE INDEX download_jobs_active_source ON download_jobs(video_id)
    WHERE operation = 'download'
    AND status IN ('queued', 'running', 'verifying', 'importing');
CREATE UNIQUE INDEX download_jobs_active_target ON download_jobs(target_key)
    WHERE operation = 'download'
    AND status IN ('queued', 'running', 'verifying', 'importing');

CREATE TABLE download_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id INTEGER NOT NULL REFERENCES download_jobs(id) ON DELETE CASCADE,
    video_id TEXT NOT NULL CHECK (length(video_id) = 11),
    title TEXT NOT NULL,
    channel TEXT,
    duration_seconds INTEGER,
    UNIQUE(job_id, video_id)
) STRICT;
-- Stable completed provenance also prevents fetching the same video again.
CREATE TABLE downloaded_sources (
    video_id TEXT PRIMARY KEY,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES audio_files(id) ON DELETE CASCADE
) STRICT;
-- Journal intent BEFORE publishing a file; recovery finalizes interrupted SQL.
CREATE TABLE download_imports (
    job_id INTEGER PRIMARY KEY REFERENCES download_jobs(id) ON DELETE RESTRICT,
    sha256 TEXT NOT NULL,
    path TEXT NOT NULL
) STRICT;
CREATE TABLE discovery_download_links (
    candidate_id INTEGER PRIMARY KEY
        REFERENCES discovery_candidates(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE
) STRICT;
-- Remember explicit clears too, so delayed imports cannot restore old likes.
CREATE TABLE recording_reaction_decisions (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
    changed_at TEXT NOT NULL,
    PRIMARY KEY(user_id, recording_id)
) STRICT;
INSERT INTO recording_reaction_decisions
    SELECT user_id, recording_id, changed_at FROM recording_reactions;
CREATE TRIGGER reaction_decision_insert AFTER INSERT ON recording_reactions
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        NEW.user_id, NEW.recording_id, NEW.changed_at
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;
CREATE TRIGGER reaction_decision_update AFTER UPDATE ON recording_reactions
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        NEW.user_id, NEW.recording_id, NEW.changed_at
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;
CREATE TRIGGER reaction_decision_delete AFTER DELETE ON recording_reactions
WHEN EXISTS(SELECT 1 FROM users WHERE id = OLD.user_id)
    AND EXISTS(SELECT 1 FROM recordings WHERE id = OLD.recording_id)
BEGIN
    INSERT INTO recording_reaction_decisions VALUES (
        OLD.user_id, OLD.recording_id,
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    ) ON CONFLICT(user_id, recording_id) DO UPDATE
        SET changed_at = excluded.changed_at;
END;

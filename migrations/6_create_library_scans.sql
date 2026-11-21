-- Persist observations separately from curated catalog metadata.
CREATE TABLE audio_file_tags (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    tags TEXT NOT NULL CHECK (json_valid(tags))
) STRICT;

CREATE TABLE library_scans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    status TEXT NOT NULL CHECK (
        status IN ('running', 'completed', 'failed', 'interrupted')
    ),
    roots TEXT NOT NULL CHECK (json_valid(roots)),
    imported INTEGER NOT NULL DEFAULT 0 CHECK (imported >= 0),
    refreshed INTEGER NOT NULL DEFAULT 0 CHECK (refreshed >= 0),
    skipped INTEGER NOT NULL DEFAULT 0 CHECK (skipped >= 0),
    errors INTEGER NOT NULL DEFAULT 0 CHECK (errors >= 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TEXT,
    message TEXT
) STRICT;

-- Enforce one active scan even when HTTP requests race.
CREATE UNIQUE INDEX library_scans_active ON library_scans((1))
    WHERE status = 'running';

CREATE TABLE library_scan_errors (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_id INTEGER NOT NULL REFERENCES library_scans(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    message TEXT NOT NULL
) STRICT;

CREATE INDEX library_scan_errors_scan_id
    ON library_scan_errors(scan_id, id);

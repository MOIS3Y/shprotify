-- One scheduler reuses existing supervised workers. All schedules are opt-in.
CREATE TABLE automation_schedules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL CHECK (kind IN ('library','discovery','metadata')),
    owner_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    feed_id INTEGER REFERENCES discovery_feeds(id) ON DELETE CASCADE,
    entity_type TEXT,
    entity_id INTEGER,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1)),
    interval_seconds INTEGER NOT NULL
        CHECK (interval_seconds BETWEEN 1 AND 2592000),
    next_run_at INTEGER,
    last_run_at INTEGER,
    last_status TEXT NOT NULL DEFAULT 'idle' CHECK (last_status IN (
        'idle','dispatching','running','completed','failed','deferred','paused'
    )),
    last_job_id INTEGER,
    failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    message TEXT,
    FOREIGN KEY (entity_type,entity_id)
        REFERENCES catalog_metadata(entity_type,entity_id) ON DELETE CASCADE,
    CHECK (
        (kind='library' AND owner_id IS NULL AND feed_id IS NULL
            AND entity_type IS NULL AND entity_id IS NULL) OR
        (kind='discovery' AND owner_id IS NOT NULL AND feed_id IS NOT NULL
            AND entity_type IS NULL AND entity_id IS NULL) OR
        (kind='metadata' AND owner_id IS NOT NULL AND feed_id IS NULL
            AND entity_type IS NOT NULL AND entity_id IS NOT NULL)
    )
) STRICT;
CREATE UNIQUE INDEX automation_library ON automation_schedules(kind)
    WHERE kind='library';
CREATE UNIQUE INDEX automation_feed ON automation_schedules(feed_id)
    WHERE kind='discovery';
CREATE UNIQUE INDEX automation_metadata
    ON automation_schedules(entity_type,entity_id) WHERE kind='metadata';
CREATE INDEX automation_due ON automation_schedules(next_run_at,id)
    WHERE enabled=1;

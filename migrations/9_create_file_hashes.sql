CREATE TABLE audio_file_hashes (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL CHECK (
        length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'
    )
) STRICT;

CREATE INDEX audio_file_hashes_sha256 ON audio_file_hashes(sha256);

-- Legacy observations need a successful full probe before duplicate lookup.
DELETE FROM audio_file_states;

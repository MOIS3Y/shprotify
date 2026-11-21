-- Properties are declared metadata until an audio probe verifies them.
CREATE TABLE audio_files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE RESTRICT,
    path TEXT NOT NULL UNIQUE CHECK (
        substr(path, 1, 1) = '/' AND
        length(CAST(path AS BLOB)) BETWEEN 1 AND 4096 AND
        instr(path, char(0)) = 0
    ),
    format TEXT NOT NULL CHECK (length(format) BETWEEN 1 AND 32),
    size_bytes INTEGER CHECK (size_bytes > 0),
    duration_ms INTEGER CHECK (duration_ms > 0),
    sample_rate_hz INTEGER CHECK (sample_rate_hz > 0),
    channels INTEGER CHECK (channels > 0),
    bit_depth INTEGER CHECK (bit_depth > 0),
    bitrate_bps INTEGER CHECK (bitrate_bps > 0)
) STRICT;

CREATE INDEX audio_files_recording_id ON audio_files(recording_id);

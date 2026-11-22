-- Only successfully probed and committed files receive a reusable snapshot.
CREATE TABLE audio_file_states (
    file_id INTEGER PRIMARY KEY REFERENCES audio_files(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (json_valid(state))
) STRICT;

ALTER TABLE library_scans ADD COLUMN unchanged INTEGER NOT NULL DEFAULT 0
    CHECK (unchanged >= 0);
ALTER TABLE library_scans ADD COLUMN force INTEGER NOT NULL DEFAULT 0
    CHECK (force IN (0, 1));

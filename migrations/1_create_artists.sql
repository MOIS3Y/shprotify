-- Names are deliberately not unique: distinct artists may share a name.
CREATE TABLE artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255)
) STRICT;

-- Accounts remain key-only until an operator explicitly sets a password.
CREATE TABLE user_passwords (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    hash TEXT NOT NULL
) STRICT;

-- Session credentials are independent of long-lived personal keys.
CREATE TABLE user_sessions (
    digest TEXT PRIMARY KEY CHECK (length(digest) = 64),
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX user_sessions_user_id ON user_sessions(user_id);
CREATE INDEX user_sessions_expiry ON user_sessions(expires_at);

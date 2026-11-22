-- Existing keys remain valid, but no account is implicitly made administrator.
ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'member'
    CHECK (role IN ('member', 'admin'));

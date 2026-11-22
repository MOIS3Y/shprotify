# Personal accounts, reactions, and favorites

Create accounts locally using the same configuration as the server:

```sh
cargo run -- --config dev/shprotify.toml user create --name alice
cargo run -- --config dev/shprotify.toml user token --name alice
cargo run -- --config dev/shprotify.toml user revoke --name alice
```

`create` returns JSON containing `user` and a new `token`. `token` issues an
additional key for an existing account. Store the returned credential: the
server persists only its SHA-256 digest, and cannot display it again. `revoke`
invalidates every key for the account, without deleting reactions/favorites.
Issuing a key afterward restores access to the same personal data. Names are
normalized to lowercase and accept 1–64 ASCII letters, digits, `.`, `_`, `-`.
Duplicate account creation fails without replacing an existing identity.

Credentials contain 256 random bits from the OS. They are long-lived bearer
keys without automatic expiry. They are printed only by the local CLI to
stdout; diagnostic logs remain on stderr. Local account commands do not need
working music roots or an HTTP listener. They require operator access to the
configured SQLite file. Use HTTPS when sending bearer keys outside loopback.

## Roles and shared operations

New accounts and accounts upgraded from earlier versions have role `member`,
even if named `admin`. Members can use their personal endpoints. Ordinary
catalog reads, search, health, and optional documentation remain public.

Set permissions locally using the configured database:

```sh
cargo run -- --config dev/shprotify.toml user role --name alice --role admin
cargo run -- --config dev/shprotify.toml user role --name alice --role member
```

Promote an existing account explicitly to provision the first administrator.
There is no implicit first-user promotion or public bootstrap endpoint. Local
operator access can always restore an administrator, including after demoting
the last one. The role command returns the user identity without issuing a key.

Only `admin` may create artists/releases/recordings/release tracks, register
file metadata, start scans, inspect scan history/status/errors, and read exact
duplicate reports. Anonymous/invalid credentials receive 401 with a Bearer
challenge. A valid member receives JSON 403 (`forbidden`) without a challenge.
Authorization occurs before body/query validation or any catalog/job mutation.

Roles are read from SQLite on every request, not embedded in the bearer key.
Promotion/demotion applies to subsequent requests with all existing keys;
account IDs, keys, reactions, and favorites remain intact. Already accepted
requests/jobs are not cancelled by a later role change. `/me` exposes the
current role; role headers, query parameters, and client claims grant no rights.
Keys may still be revoked independently of the role.

Password login and HTTP account management remain subsequent features.

Set `PERSONAL_API_KEY` in your client environment to the delivered key:

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  http://127.0.0.1:3000/api/v1/me
curl -X PUT -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Content-Type: application/json' -d '{"reaction":"like"}' \
  http://127.0.0.1:3000/api/v1/me/recordings/1/reaction
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  http://127.0.0.1:3000/api/v1/me/recordings/1/reaction
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/me/favorites?limit=50&offset=0'
```

Use `dislike` in the PUT body for a negative assessment. DELETE on the same
reaction URL clears the assessment and returns `reaction: null` and
`changed_at: null`. PUT requires an explicit `like` or `dislike`; null and
unknown fields are rejected. Unknown recordings return 404, including DELETE.
Neutral assessments of existing recordings return 200 with null fields.

Exactly one current assessment exists per account and `Recording`. Alternative
encodings already sharing a recording share its reaction; different recording
IDs remain independent. Repeating the same reaction preserves its timestamp.
Dislike and DELETE immediately exclude the recording from favorites. Liking
again after either starts a new favorite with a new UTC timestamp. No reaction
history or aggregate score is maintained at this stage.

`/me/favorites` is the account's special dynamic favorites playlist, available
even when empty. It contains the current likes directly, with `recording_id`,
current catalog `title`, and UTC `added_at`. There is no copied ordinary
playlist to synchronize. Entries sort by addition time descending; equal
millisecond timestamps use recording ID descending as a deterministic tie.
Pagination defaults to 50 entries, limited to 1–100, with nonnegative offset.

Personal routes never accept a user ID as authority. Credentials determine the
account; missing, malformed, duplicate, or revoked credentials return JSON 401
with a Bearer `WWW-Authenticate` challenge. Storage failures remain safe 500
responses. Swagger's Authorize control accepts the raw token for these routes.

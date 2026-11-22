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

## Password login and sessions

Set an existing account's password locally. The default mode prompts twice
without echo, writing prompts to stderr; the password never enters argv or
configuration:

```sh
cargo run -- --config dev/shprotify.toml user password --name alice
```

For automation, pass `--password-stdin` and redirect a protected secret file
or pipe a secret manager's output. Input is read through EOF with a size limit;
only one final LF or CRLF is removed. No password argument or environment
variable is accepted. Passwords must contain 12–1024 UTF-8 bytes without control
characters. Spaces and Unicode are preserved exactly, without normalization.
Interactive entry requires a terminal. Success returns the user identity and
`password_set: true`, without any credential.

Passwords are stored as salted Argon2id v19 PHC strings (19 MiB memory,
two passes, one lane). Every setting generates a new salt and atomically
revokes all password sessions for that account. Personal keys and preferences
remain intact. Existing accounts remain key-only until a password is set.
There is no public registration or unauthenticated password-reset endpoint.
Administrators can reset passwords through [account management](users.md).

Send JSON to `POST /api/v1/auth/login`:

```json
{"name":"alice","password":"your actual password"}
```

A successful response has this shape:

```json
{
  "access_token": "shs_<64 random hex characters>",
  "token_type": "Bearer",
  "expires_at": 1800000000
}
```

`expires_at` is the absolute expiration time in Unix seconds. Each login
creates an independent session lasting 30 days by default; activity does not
extend it.
Configure the lifetime in the server TOML:

```toml
[auth]
session_ttl_seconds = 2592000 # 30 days; use 86400 for one day
```

The value must be a positive integer in seconds (1–4294967295). Environment
override: `SHPROTIFY_AUTH__SESSION_TTL_SECONDS`. Restart the server to apply
changes. The setting affects newly issued sessions only: existing sessions
retain their stored expiration, including older 24-hour sessions. Personal
API keys remain valid until explicitly revoked.

Only a SHA-256 token digest is persisted. Sessions survive server restarts and
read current roles on each request. The response carries `Cache-Control:
no-store` and `Pragma: no-cache`. Use HTTPS outside loopback for both login and
authenticated requests. Avoid placing passwords in shell history or logs.

Use `access_token` in `Authorization: Bearer <token>` for the same personal and
administrative endpoints as a personal key. In Swagger, call login and paste
the delivered token into Authorize. This endpoint returns JSON; it does not
set a cookie or automatically authorize Swagger.

Unknown names, key-only accounts and incorrect passwords receive the same
JSON 401 response. Password verification runs off the async workers with at
most two concurrent Argon2 tasks per process, including tasks whose requests
were cancelled. At capacity, login returns JSON 429 with `Retry-After: 1`.
The login JSON body is limited to 4096 bytes; unknown fields are rejected.

`POST /api/v1/auth/logout` with the session's Bearer token returns 204 and
revokes only that session. Other sessions and personal keys remain usable.
Expired, revoked or absent session tokens receive 401; personal keys also
receive 401 on logout and are revoked through `user revoke` instead. That CLI
command revokes personal keys only. To invalidate every password session,
set a new password through `user password` or use the administrative
[session revocation endpoint](users.md).

Token refresh, public registration and browser cookie transport are deferred.
After expiry, clients must log in again.

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

Administrators can also manage accounts through the
[account management API](users.md), including passwords, roles and credentials.

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

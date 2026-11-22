# Administrative account management

All routes below require an active `admin` personal key or password session.
Set `ADMIN_API_KEY` in the client environment to the raw credential and send
`Authorization: Bearer $ADMIN_API_KEY`. Use HTTPS outside loopback, including
when delivering account passwords and retrieving newly issued keys.

Provision the first account and administrator locally, as described in
[personal.md](personal.md). There is no public registration or first-user
promotion. Members cannot list accounts, change their own role/password, issue
keys, or revoke credentials using these administrative endpoints. Requests
without valid credentials receive 401; members receive 403. Authentication
precedes payload, path, and query validation.

| Method | Path under `/api/v1` | Behavior |
| --- | --- | --- |
| POST | `/users` | Create a password account; return 201 and Location. |
| GET | `/users` | List account identities with bounded pagination. |
| GET | `/users/{id}` | Read an account identity. |
| PUT | `/users/{id}/role` | Replace the role; return the updated identity. |
| PUT | `/users/{id}/password` | Reset the password and revoke all sessions. |
| POST | `/users/{id}/keys` | Issue an additional permanent personal key. |
| DELETE | `/users/{id}/keys` | Revoke all personal keys; return 204. |
| DELETE | `/users/{id}/sessions` | Revoke all password sessions; return 204. |

## Provisioning and discovery

Send this JSON to `POST /api/v1/users`:

```json
{"name":"alice","password":"a strong password"}
```

The optional `role` defaults to `member`; `admin` must be explicit. Null and
unknown roles are rejected. Names use the same normalization and validation as
the CLI: trim, ASCII lowercase, 1–64 letters, digits, `.`, `_`, `-`. A duplicate
normalized name returns 409 without modifying the existing account. Password
validation, Argon2id parameters, and session lifetime follow
[personal.md](personal.md). Account and password are created atomically.
No permanent key is issued implicitly, and no session is created for the new
account. The user can immediately log in through `/api/v1/auth/login`.

The response contains only `id`, `name`, and `role`, with a Location header
such as `/api/v1/users/3`. GET operations return the same identity fields,
never password hashes or credential digests. Listing sorts by ID ascending,
with `limit=50` and `offset=0` by default; limit must be 1–100 and offset
nonnegative. The page has `items`, `limit`, and `offset`.

```sh
curl -H "Authorization: Bearer $ADMIN_API_KEY" \
  'http://127.0.0.1:3000/api/v1/users?limit=50&offset=0'
curl -H "Authorization: Bearer $ADMIN_API_KEY" \
  http://127.0.0.1:3000/api/v1/users/3
```

## Roles and passwords

PUT `/api/v1/users/3/role` with `{"role":"admin"}` or `{"role":"member"}`.
Role changes apply to subsequent requests with every existing key/session.
Already accepted operations are not cancelled. Self-demotion and demotion of
the last administrator are allowed, consistently with the CLI. If necessary,
restore administrator access locally using `user role --name ... --role admin`.

PUT `/api/v1/users/3/password` with `{"password":"another strong password"}`.
A successful reset atomically replaces the password and revokes all current
password sessions for that account. Personal keys and personal data remain
intact. No password, hash, or token is returned; the response is the identity.
A login already verifying the old password cannot issue a session after reset.

JSON request bodies are limited to 4096 bytes. Unknown fields and invalid
field values return 422; malformed JSON returns 400, excessive bodies 413,
and missing JSON content type 415. Password hashing uses the same bounded
worker pool as login and returns 429 with `Retry-After: 1` at capacity.

## Credential issuance and revocation

POST `/api/v1/users/3/keys` requires no body. It returns 200 with
`{"user":{"id":3,"name":"alice","role":"member"},"token":"shp_..."}`.
The raw key is delivered once with `Cache-Control: no-store` and `Pragma:
no-cache`. Copy it securely; it cannot be retrieved again. The database stores
only its SHA-256 digest. Issuance adds a key without invalidating existing keys
or sessions. An administrator can issue credentials for any target account,
so protect administrator credentials accordingly.

DELETE `/api/v1/users/3/keys` invalidates all of that account's permanent keys,
including the caller's key when targeting oneself. Password sessions remain
usable. DELETE `/api/v1/users/3/sessions` invalidates all password sessions,
including the caller's session when targeting oneself; personal keys remain
usable. Neither operation disables password login or changes the password.
To invalidate both credential types, use both operations or reset the password
and revoke the keys.

Both DELETE operations return 204 with no body even when the existing account
has no credentials of that type. Unknown target accounts return 404; malformed
or nonpositive IDs return 400. Reactions/favorites and the shared catalog are
preserved. Account deletion, renaming, suspension, and self-service password
changes are not part of these administrative operations.

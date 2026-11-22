# Owned playlists and instance publication

All `/me/playlists` operations are scoped to the authenticated account using
a personal key or password session. Members and administrators have identical
rights over their own playlists. Another user's ID returns 404 on these owner
routes even after publication. Separate read-only publication routes below
are available to authenticated instance members.

| Method | Path under `/api/v1` | Behavior |
| --- | --- | --- |
| POST | `/me/playlists` | Create an empty playlist; return 201 and Location. |
| GET | `/me/playlists` | Page the current account's playlists in ID order. |
| GET | `/me/playlists/{id}` | Read playlist metadata and its version. |
| PUT | `/me/playlists/{id}` | Replace name/description with a version check. |
| DELETE | `/me/playlists/{id}?version=N` | Delete using the current version. |
| GET | `/me/playlists/{id}/tracks` | Page ordered recording occurrences. |
| PUT | `/me/playlists/{id}/tracks` | Atomically replace the entire ordered list. |
| PUT | `/me/playlists/{id}/visibility` | Publish/unpublish with a version check. |
| GET | `/playlists` | Page all currently published playlists. |
| GET | `/playlists/{id}` | Read published metadata. |
| GET | `/playlists/{id}/tracks` | Page published track occurrences. |

Create with `{"name":"Road","description":"Night driving"}`. Name is trimmed,
required, 1–255 Unicode characters, without controls. Description defaults to
empty, is trimmed and accepts up to 2000 Unicode characters; newlines and tabs
are allowed. Other control characters are rejected. Names need not be unique,
even within the same account. Metadata responses have `id`, `name`,
`description`, `is_public`, `version`, `created_at`, and `updated_at`;
timestamps are UTC ISO 8601 with millisecond precision. The owner is derived from credentials.

## Editing and concurrency

A new playlist has version 1 and `is_public: false`. Supply its last observed
version in every PUT body, and as a query parameter for DELETE. Effective
metadata, visibility or track changes
increment the same version. A stale version returns 409 without modifying
anything. Fetch the current state and reconcile before retrying. This includes
retries after a lost response: an applied edit is not silently re-applied over
a newer state. Replacing unchanged metadata or an identical list with the
current version is a no-op and preserves version/timestamps.

Replace metadata with `{"version":1,"name":"Road","description":"Updated"}`.
Omitting description in PUT clears it. Metadata edits preserve track order.
`updated_at` can tie for fast edits; use the version for concurrency control.

Replace contents with this body, in the desired playback order:

```json
{"version":2,"recording_ids":[1,2,1]}
```

The array is the **whole replacement**, not an append operation. Add or remove
IDs in the array to append/remove occurrences, rearrange it to reorder, or
send `[]` to clear. The maximum is 10000 positive recording IDs per playlist.
All recordings must exist in the shared catalog, but need not have a currently
available local audio file. An unknown recording returns 422, retaining the
previous order, metadata, version and timestamps. SQLite changes are atomic,
including rollback on failures during insertion.

Repeated recording IDs are allowed, representing separate occurrences at
one-based positions. There are no stable track-entry IDs in this interface;
positions identify occurrences within a playlist version. Clients editing a
paged playlist must collect its whole ordered list first. Every content page
includes `version`: if pages differ in version, reload before constructing the
replacement. The PUT version check also rejects edits that raced a later
change after the final page was fetched.

GET `/me/playlists/{id}/tracks` returns `version`, `items`, `limit`, `offset`.
Each item has `position`, `recording_id`, current catalog `title`. Positions
are dense from 1 and sort ascending. Catalog title corrections are reflected
when read, without changing playlist version. GET `/me/playlists` returns
`items`, `limit`, `offset`, containing metadata only. Both lists default to
limit 50, permit 1–100, and require a nonnegative offset. They return empty
items past the end; an existing empty playlist also has an empty track page.

```sh
curl -X POST -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Content-Type: application/json' -d '{"name":"Road"}' \
  http://127.0.0.1:3000/api/v1/me/playlists
curl -X PUT -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"version":1,"recording_ids":[1,2,1]}' \
  http://127.0.0.1:3000/api/v1/me/playlists/1/tracks
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/me/playlists/1/tracks?limit=50&offset=0'
curl -X DELETE -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/me/playlists/1?version=2'
```

Deletion returns 204 with no body and removes only the playlist and its
occurrences. Recordings, source files, history, reactions and favorites remain
intact. Recordings referenced by playlists cannot be deleted directly from
SQLite until those references are removed, preserving playlist order.

## Publishing inside the instance

Publication is explicit. For an owned playlist currently at version 2:

```sh
curl -X PUT -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"version":2,"is_public":true}' \
  http://127.0.0.1:3000/api/v1/me/playlists/1/visibility
```

The response has `is_public: true` and version 3. Both the version and boolean
are required; null/string booleans are rejected. Existing playlists upgraded
from earlier versions stay private without changing their versions or tracks.

Publication makes the name, description, metadata and ordered recordings
readable by **every authenticated account on this instance**. Anonymous reads
still receive 401. Use GET `/api/v1/playlists` to discover published lists,
GET `/api/v1/playlists/{id}` for metadata, and GET
`/api/v1/playlists/{id}/tracks` for contents. GET routes also support HEAD.
The catalog includes one's own published playlists and excludes private ones.
Its pagination/order matches the owner list; track pages have the same
position and version fields as the owner endpoints.

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/playlists?limit=50&offset=0'
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  http://127.0.0.1:3000/api/v1/playlists/1/tracks
```

Readers receive no editing rights, even administrators. All writes remain on
the owner's `/me/playlists` routes, where another account still receives 404.
Publication does not expose owner IDs, credentials or other personal data.
It does not bypass the authentication/filesystem policy of audio streaming.

Publication is live: subsequent owner name/description/order edits appear on
the shared read routes and preserve `is_public`. It is not a copied playlist.
Visibility changes share the mutation version, so concurrent publication and
ordinary edits cannot silently overwrite each other. Repeating the current
visibility with the current version is a no-op. A stale version receives 409,
including for an otherwise unchanged publication request.

Set `is_public: false` using the current version to stop publication. New
shared metadata/track reads return 404 for everyone, including the owner; the
owner can still read and edit via `/me/playlists`. Contents are retained.
Deleting a published playlist also removes it from discovery/shared reads.

Visibility and content are read in one SQLite snapshot. An already accepted
read may finish with its old public snapshot, but it cannot combine an old
public flag with a newer private track list. Shared responses use
`Cache-Control: private, no-store`. Unpublishing blocks new access; previously
received data cannot be recalled from another client's memory or saved copy.

## Favorites, privacy and validation

The dynamic [favorites collection](personal.md) remains a separate feature.
A normal playlist named "Favorites" is still an ordinary playlist; likes do
not synchronize it. Creating/updating/deleting playlists never alters likes.
No ordinary playlist is created automatically from existing favorites.

Revoking credentials does not erase playlists. Successful responses use
`Cache-Control: private, no-store`. All routes authenticate before parsing
input. Invalid credentials receive 401 with a Bearer challenge. Unknown JSON
fields such as `user_id` or `public` are rejected. `is_public` is accepted only
by the visibility endpoint, not creation or metadata PUT. Per-occurrence
mutations and playback queues remain subsequent features.

Invalid JSON fields/domain values return 422; malformed JSON, IDs, queries or
missing/nonpositive DELETE version return 400. Missing JSON content type is
415; request bodies exceeding 256 KiB receive 413. Unexpected storage errors
use the safe JSON 500 envelope. OpenAPI describes the routes, bounded arrays
and required version preconditions.

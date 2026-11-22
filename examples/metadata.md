# Metadata corrections, enrichment and artwork

All `/api/v1/metadata/...` routes require an administrator key or session.
The existing catalog remains readable with its current permission policy.
`GET /api/v1/artwork/{kind}/{id}` is public, like catalog titles.
`kind` is `artist`, `release` or `recording`, using singular names.
A release here is a specific MusicBrainz release edition, not a release group.

## Manual corrections

Read `GET /api/v1/metadata/recording/12` to obtain the current version, name,
ordered `artist_ids`, `release_kind`, `mbid`, last metadata `source`, and current
`artwork_digest`/`artwork_source`. The `name` field means artist name or title
for recordings/releases. New and migrated entities start at version 1.
The baseline source `existing` means original attribution is unavailable.
Existing scanner Recording IDs are preserved without merging local entities.

```sh
curl -X PUT "$BASE/api/v1/metadata/recording/12" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"version":1,"name":"Correct title","artist_ids":[3,8],
       "mbid":null}'
```

This is complete replacement metadata. Name/title is trimmed, 1–255 Unicode
characters, without controls. Credits allow at most 100 distinct positive,
existing artist IDs, preserving order. Artists require empty credits;
`release_kind` is required for releases and must be absent/null otherwise.
Null/omitted `mbid` clears the confirmed link; provided IDs must be UUIDs.
A confirmed MBID can identify only one local entity per namespace. A conflict
is reported as `422`; no name-based merging or transfer of personal data occurs.

Text corrections, confirmed candidate applications and cover changes all
advance one entity version. Every accepted edit creates a revision, including
reaffirming unchanged values. Stale edits receive `409`; reload before retrying.
`GET /metadata/{kind}/{id}/revisions?limit=50&offset=0` returns a bounded audit
page, newest first, including the initial snapshot before its first change.
Each revision contains resulting fields, image digest, source, acting account
and server time. It does not retain old image bytes or implement undo.

Changes apply immediately to the catalog and its search index. Audio bytes,
embedded tags, file paths and personal Recording references remain unchanged.
Rescanning existing paths keeps curated titles/credits and file associations.
For recordings, confirming/changing/clearing MBID also changes matching for
future new file paths. It does not reassign files already in the catalog.

## Background suggestions

Enable external calls explicitly in `[metadata]` in the full configuration
example. Set a meaningful `user_agent` with application/version and operator
contact. Manual corrections, uploads and embedded extraction work with external
metadata disabled. No network calls or fingerprint programs run during normal
scanning or server startup.

```sh
curl -X POST "$BASE/api/v1/metadata/recording/12/jobs" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"action":"search","query":"Recording title"}'
```

The response is `202` with a job and `Location: /api/v1/metadata/jobs/{id}`.
Poll that location to obtain `{job, candidates}`. Lifecycle values are
`running`, `completed`, `failed`, `interrupted`; completion may have no matches.
Each candidate has `id`, `job_id`, `state` and a normalized `proposal`, including
source, MBID, title/name, context, ordered external credits and provider score.
Scores are provider-specific relevance values, not acceptance probabilities.
Candidate states are `pending`, `applied` or `rejected`.

| Action | Required options | Result |
| --- | --- | --- |
| `search` | Optional `query`, defaults to current name/title | At most 10 MusicBrainz candidates |
| `lookup` | `mbid` | One exact MusicBrainz entity |
| `embedded_artwork` | Associated `file_id`, recording/release target | One embedded front-cover image candidate |
| `cover_art` | Release target with confirmed MBID | CAA front-500 image candidate |
| `fingerprint` | Recording target, associated `file_id`, configured key/program | At most 5 Recording candidates |

Options unrelated to an action are rejected. Search text is a literal phrase;
full Lucene query syntax is not exposed. Search/lookup cover artists, releases
and recordings; they do not import a complete release track list or guess local
relationships from names. Search suggestions never write catalog metadata.

Apply a pending candidate explicitly:

```sh
curl -X POST "$BASE/api/v1/metadata/candidates/42" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"version":2,"name":false,"credits":true}'
```

For metadata candidates, identity is always linked; `name` and `credits`
explicitly select which fields to replace. False preserves current manual
values. Release classification remains unchanged. Credit import reuses local
artists only by confirmed MusicBrainz ID and otherwise creates an artist;
identical names never cause a merge. Renaming an existing credited artist is
its own explicit correction. The complete decision is transactional.

For artwork candidates, both switches must be false; accepting replaces the
cover and preserves text. Preview bytes first through
`GET /metadata/candidates/{id}/artwork`. Reject a pending candidate with
`DELETE /metadata/candidates/{id}`. Decided candidates cannot be applied again.
An entity version alone is not permission to apply another entity's candidate:
the candidate permanently belongs to its original target.

One metadata job runs per instance, independently of library scanning. New
admission while busy returns `409`. Accepted work survives HTTP disconnects;
shutdown waits for it. After a crash, unfinished work becomes `interrupted`,
with no automatic external replay. Completed suggestions remain available.
External requests have connection/total deadlines, bounded bodies, HTTPS-only
known redirect destinations and shared pacing of at most one request start per
second. MusicBrainz redirects are reported as failed jobs rather than followed
without pacing. Retry failed jobs explicitly after inspecting server logs.
Jobs retain their target, action and candidates; request search text is not
stored as a separate query history. Discover persisted jobs through `GET /metadata/jobs?limit=50&offset=0`.
Pruning and retry scheduling are not part of this stage.

## Artwork

Upload original encoded image bytes (not JSON/base64):

```sh
curl -X PUT "$BASE/api/v1/metadata/release/7/artwork?version=3" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: image/jpeg' --data-binary @cover.jpg
```

Only decodable PNG/JPEG is accepted: maximum 5 MiB encoded, maximum 4096 pixels
per dimension and bounded decoder memory. Type comes from content, not header
or extension. Malformed/unsupported images receive `422`; oversized bodies
receive `413`. Two simultaneous upload decoders are allowed; busy receives
`409`. Embedded source files are limited to 512 MiB and opened through the same
configured-root, regular-file, no-symlink checks as streaming. Normal scanning
continues to skip embedded artwork; extraction is an explicit job.

Artwork is stored locally as a bounded SQLite BLOB, simplifying atomic edits
and database backups. There is one cover per entity; a release cover is not
automatically copied to every recording/artist. CAA fetches use the configured
release identity, without accepting arbitrary remote URLs. No image resizing,
multiple variants or filesystem sidecar discovery is performed in this stage.

Public image reads support GET/HEAD, content-derived MIME, `nosniff`, a strong
SHA-256 ETag and `If-None-Match`/`304`. Mutable URLs require revalidation
(`public, no-cache`); administrative previews use `private, no-store`.
Delete via `DELETE /metadata/{kind}/{id}/artwork?version=N`.

## Correcting associations

If a source file or release track was matched to the wrong Recording:

```sh
curl -X PUT "$BASE/api/v1/metadata/links/file/9" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"expected_recording_id":12,"recording_id":34}'
```

The link kind is `file` or `release_track`. Its stable ID comes from existing
catalog endpoints. Compare-and-set returns `409` if the current Recording
changed, `404` for an unknown link, `422` for an unknown destination Recording.
This changes only that association and records attribution in
`metadata_link_revisions`. For release tracks, release/disc/position are kept.
For files, path, source tags and observations are kept. Existing personal likes,
queues, playlists and history are not moved to another recording. Streaming
already in progress may finish using its previously authorized open descriptor.
This is correction of a link, not global recording merge or deduplication.

## Optional acoustic identification

Install Chromaprint's `fpcalc` and configure an explicit executable path plus
an AcoustID **application** key. The feature is disabled without these settings
and `metadata.enabled = true`. It sends a fingerprint and duration to AcoustID,
then resolves returned Recording IDs through MusicBrainz. Audio bytes are never
uploaded. No fingerprint submissions or user AcoustID credentials are used.

The process gets a securely opened descriptor on stdin, with no shell or reopening of the original source path. On Linux, the
child reads `/proc/self/fd/0` to preserve seek support. Analysis is capped at 120 seconds of media, execution at 60 seconds,
and output at 128 KiB. Missing procfs, unsupported codecs or decoder failures produce a failed job
and leave all catalog data unchanged.
Matches use the same review/apply workflow as text search; nothing is merged
or accepted automatically. AcoustID can return no linked MusicBrainz recording.

Provider contracts: [MusicBrainz API](https://musicbrainz.org/doc/MusicBrainz_API),
[CAA API](https://musicbrainz.org/doc/Cover_Art_Archive/API),
[AcoustID API](https://acoustid.org/webservice).

# Read-only collection scan

Set `[library].directories` in a copy of `shprotify.toml`, then run:

```sh
cargo run -- --config dev/shprotify.toml scan > dev/scan.jsonl
```

The command uses the same TOML/environment settings as the server. Relative
roots resolve against the working directory. It reads existing files without
opening SQLite, starting HTTP, or changing the collection or catalog. Running
the server without a subcommand still only validates the configured roots.

Each stdout line is an independent JSON object. Logs go to stderr. Records
arrive in filesystem iteration order; consumers must not depend on ordering.

- `type: "audio"`: `file` contains the absolute `path`, detected `format`,
  source `tags`, and measured `properties`.
- `type: "error"`: `path` and `message` describe an entry that could not be
  read. Other entries are still processed.
- `type: "summary"`: final `audio_files`, `skipped`, and `errors` counts.

An empty roots list succeeds with zero counts. Any reported entry error makes
the final exit status nonzero. Invalid configured roots fail before traversal;
report output failures stop it immediately. Such failures can leave a partial
report without a final summary.

Format detection uses file contents through Lofty, so a WAV named `song.flac`
is reported as `wav`. Unknown content is skipped, but unrecognized content
with a supported audio extension is an error. A readable header and metadata
do not guarantee that the entire audio stream can be decoded.

The scanner selects the primary tag, falling back to the first available tag.
Title, artist, album, album artist, track number, and disc number retain their
source values. Missing tags and unknown properties are `null`; names are not
inferred from filenames, and artist credits are not split into identities.
Duration is in milliseconds, size in bytes, sample rate in hertz, and audio
bitrate in bits per second. Artwork is not loaded.

Configured symlink roots are resolved during validation. Links inside those
roots, including links to files, are skipped along with special files. This
avoids ordinary link loops and traversal outside the roots; it does not
provide isolation against concurrent filesystem replacement. Non-UTF-8 paths,
control characters, and paths over 4096 bytes are reported as errors.

## Background indexing through HTTP

With the server running, use an administrator key to index configured roots.
Set `PERSONAL_API_KEY` to that key (see `personal.md` for local role setup).
All scan endpoints and duplicate reports require `admin`; members receive 403.

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -X POST http://127.0.0.1:3000/api/v1/library/scans
```

The response is `202 Accepted` with the accepted job snapshot and a `Location`
header such as `/api/v1/library/scans/1`. No request body or client-supplied
directory is needed. Filesystem and audio work run outside HTTP handlers and
async worker threads. A second active scan returns `409 Conflict`.

### Exact duplicate reports

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/library/duplicates?limit=50&offset=0'
```

Each item contains `sha256`, `file_id`, `recording_id`, and `path`. The page
is bounded by files (1–100, default 50), ordered by digest and file ID; one
digest group can span pages. Only digests observed at multiple paths appear.
Background and CLI probes hash the complete file, including tags/container
bytes, in bounded memory. CLI reports retain their existing JSON format.

Hashes commit with file metadata and filesystem observations. Failed probes
invalidate previous hashes; unchanged files reuse committed observations.
After upgrading, old filesystem snapshots are invalidated once so the next
background scan computes missing hashes. Reports describe the last successful
observations: missing paths are retained, and this endpoint does not recheck
the filesystem. Files and recording identities are never automatically merged
or removed. Different encodings and differently tagged copies require separate
musical identity matching, which remains subsequent work.

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  http://127.0.0.1:3000/api/v1/library/scans/1
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/library/scans/1/errors?limit=50&offset=0'
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/library/scans?limit=50&offset=0'
```

Jobs persist their canonical `roots`, timestamps, `status`, and counters:

- `imported`: new paths committed to the catalog.
- `refreshed`: existing paths whose measured properties/tags were refreshed.
- `unchanged`: known audio files whose last successful probe can be reused;
  available when traversal completes.
- `errors`: filesystem/probe failures, validation errors, and matching issues.
- `skipped`: unsupported files, symlinks, and special files; available when
  traversal completes.

`running` changes to `completed` after traversal, even when individual entries
have errors. Unexpected storage/worker failures produce `failed`, with details
restricted to server logs. Each successfully indexed file is committed with
its counter independently; a later failure preserves that progress. Graceful
shutdown waits for accepted work. After a crash/restart, unfinished jobs become
`interrupted`; history and committed files remain. Jobs are not automatically
resumed against possibly changed configuration. Start a new scan to retry.

### Incremental and forced scans

Background scans compare each regular file with its last successfully
committed filesystem observation: size, device, inode, and nanosecond mtime
and ctime. Matching files increment `unchanged` without parsing audio or
rewriting their tags/properties. The final `unchanged` count is separate from
`skipped`, which continues to mean unsupported files, links, and special files.
Imported/refreshed/error progress is durable per file; `unchanged` and `skipped`
are finalized when traversal succeeds, rather than writing on every cache hit.

New files, files imported before this feature, and manual registrations have
no reusable observation and are probed once. Changed files update measured
properties and source tags while keeping existing catalog identities and
curated associations. Observations commit in the same transaction as those
updates. Failed probes invalidate old observations so subsequent scans retry;
unsupported files are not cached. A file changed or replaced during probing
is reported as an error instead of committing an inconsistent observation.

The open file and its path are checked after probing. This cannot freeze a
live collection: changes after the last check are observed on a later scan.
Filesystem timestamps are a fast heuristic, not a content hash or duplicate
detector; coarse or stale filesystem metadata can hide changes. To bypass the
optimization and reread every file, run:

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -X POST 'http://127.0.0.1:3000/api/v1/library/scans?force=true'
```

`force` defaults to `false` and is persisted in the job report. Invalid or
unknown query options return `400`. Forced scans still preserve curated
catalog metadata and do not retry association matching on existing paths.
The read-only CLI always performs a full probe without opening SQLite; its
JSON Lines format does not include internal filesystem observations.

### Catalog matching policy

Paths identify physical files. Without a matching explicit identity, a new
path creates an independent recording
and its file atomically. The recording title comes from the embedded title,
trimmed and validated; a missing/blank title falls back to the filename stem.
Invalid new titles reject that file rather than silently truncating metadata.

An existing path retains its file ID, recording link, title, artist credits,
and release membership. Measured file properties and observed source tags are
refreshed; unavailable measurements replace old declared values with null.
This also applies to files previously registered manually through the API.
Files indexed before directory-local matching was introduced are preserved
the same way: refreshing them does not backfill or rewrite associations. Those
older rows cannot reliably be distinguished from curated registrations.

Names alone do not identify musical entities: distinct paths are not merged
because their titles, artist credits, albums, or contents appear equal.
For newly imported paths, associations follow these conservative rules:

- A nonblank `artist` supplies one recording credit. Compound names such as
  `A feat. B` or `A; B` stay intact; no artist names are split.
- Credits reuse scanner-created artist IDs only within the immediate parent
  directory and only for exactly equal trimmed source names. Manual artists
  are not selected by name. There is no case folding or global name matching.
- A nonblank `album` supplies a release. Releases reuse scanner-created IDs
  by directory, trimmed album title, and explicit `album_artist` (or its
  absence). The kind is `other`: an album tag does not identify a release type.
- Only explicit `album_artist` supplies a release credit. Missing album-artist
  tags do not fall back to track performers, so compilations can share one
  release even when individual performers differ. A missing recording artist
  does not fall back to the album artist either.
- A release track requires both an album and a positive `track_number`.
  A missing disc number means disc 1. Missing track numbers are not inferred
  from filenames or traversal order. Zero track/disc numbers are invalid.

Names/titles are limited to 255 Unicode scalar values without controls. Blank
matching tags mean absent values. Invalid optional matching metadata skips
associations for that file, while its recording, file, and original tags are
still imported. The issue and its counter commit with the file transaction.

An occupied release/disc/track slot keeps its current recording. The new file
and independent recording still import, with a matching issue explaining the
missing release-track link. The first discovered file claims an empty slot;
filesystem iteration order is unspecified. Alternative encodings are not
automatically merged. A file can therefore contribute to both `imported` and
`errors`; these counters must not be summed as disjoint outcomes.

The directory boundary separates editions and ambiguous names, including
different disc subdirectories. It can produce separate artist/release IDs for
the same music in different directories. Cross-directory identity matching
and multi-directory release grouping need stronger evidence in later work.
Persistent source keys remain stable when catalog names are corrected, so new
files with the original tags reuse those IDs without undoing manual changes.

Original tags remain available without overriding curated catalog metadata:

```sh
curl http://127.0.0.1:3000/api/v1/recordings/1/files/1/tags
```

The tag endpoint returns `404` for an unknown/wrongly scoped file or a manual
registration never observed by background indexing. The read-only CLI does
not persist observations. Scans never remove catalog entries for missing files
or move, rename, write, or delete collection files. Global musical identity
matching and cross-format duplicate detection remain future work.

### Alternate encodings with explicit recording identities

The optional `recording_mbid` source tag exposes MusicBrainz Recording ID.
For new paths, one valid UUID-shaped recording ID reuses the scanner-owned
recording mapped to that ID, across formats and directories. UUID hex case
and surrounding whitespace are normalized; names/durations never establish
identity. Recording IDs are distinct from MusicBrainz release-track IDs.

The first file creates the recording and credits. Further encodings keep its
curated title and credits and store their own paths, properties, tags, and
hashes. Release associations still follow directory-local rules. An occupied
slot already pointing to the same recording is accepted without an issue.
Different releases can reference the shared recording at different positions.

Absent IDs keep files independent. Malformed, nil, or multiple IDs produce an
issue but still import an independent recording and file. Identity mappings
commit atomically with file metadata and survive restarts. Existing paths,
manual registrations, and legacy imports retain their links even if later
source tags change; no existing recordings are merged or backfilled.

This policy trusts explicit source tags; it does not verify remote MusicBrainz
entities or compare decoded audio. Acoustic matching is future work.

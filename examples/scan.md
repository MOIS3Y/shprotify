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

With the server running, start indexing its configured collection roots:

```sh
curl -X POST http://127.0.0.1:3000/api/v1/library/scans
```

The response is `202 Accepted` with the accepted job snapshot and a `Location`
header such as `/api/v1/library/scans/1`. No request body or client-supplied
directory is needed. Filesystem and audio work run outside HTTP handlers and
async worker threads. A second active scan returns `409 Conflict`.

```sh
curl http://127.0.0.1:3000/api/v1/library/scans/1
curl 'http://127.0.0.1:3000/api/v1/library/scans/1/errors?limit=50&offset=0'
curl 'http://127.0.0.1:3000/api/v1/library/scans?limit=50&offset=0'
```

Jobs persist their canonical `roots`, timestamps, `status`, and counters:

- `imported`: new paths committed to the catalog.
- `refreshed`: existing paths whose measured properties/tags were refreshed.
- `errors`: isolated filesystem/probe or catalog validation failures.
- `skipped`: unsupported files, symlinks, and special files; available when
  traversal completes.

`running` changes to `completed` after traversal, even when individual entries
have errors. Unexpected storage/worker failures produce `failed`, with details
restricted to server logs. Each successfully indexed file is committed with
its counter independently; a later failure preserves that progress. Graceful
shutdown waits for accepted work. After a crash/restart, unfinished jobs become
`interrupted`; history and committed files remain. Jobs are not automatically
resumed against possibly changed configuration. Start a new scan to retry.

### Initial catalog matching policy

Paths identify physical files. A new path creates an independent recording
and its file atomically. The recording title comes from the embedded title,
trimmed and validated; a missing/blank title falls back to the filename stem.
Invalid new titles reject that file rather than silently truncating metadata.

An existing path retains its file ID, recording link, title, artist credits,
and release membership. Measured file properties and observed source tags are
refreshed; unavailable measurements replace old declared values with null.
This also applies to files previously registered manually through the API.

Names alone do not identify musical entities: distinct paths are not merged
because their titles, artist credits, albums, or contents appear equal. Artist,
release, and track associations are not inferred yet. Original tags remain
available separately without overriding curated catalog metadata:

```sh
curl http://127.0.0.1:3000/api/v1/recordings/1/files/1/tags
```

The tag endpoint returns `404` for an unknown/wrongly scoped file or a manual
registration never observed by background indexing. The read-only CLI does
not persist observations. Scans never remove catalog entries for missing files
or move, rename, write, or delete collection files. Musical identity matching,
duplicate detection, and efficient change detection remain subsequent work.

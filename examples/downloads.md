# Experimental YouTube acquisition

Shprotify searches YouTube and downloads a source explicitly selected by an
authenticated member or administrator. It invokes a separately updatable
`yt-dlp` executable; there is no stable official audio-download API and no
availability guarantee. YouTube changes, access restrictions and missing
runtime dependencies can make individual jobs fail. Only acquire material
you are entitled to store and share within your instance.

The Nix application wrapper and development shell provide yt-dlp (including
EJS), FFmpeg and Deno. Outside Nix, install these tools yourself or configure
trusted server paths under `[downloads.youtube]`. No self-update, remote EJS
component downloads, ambient yt-dlp configuration, browser cookies, netrc,
arbitrary flags or arbitrary source URLs are accepted by Shprotify.

## Enable acquisition

Copy `examples/shprotify.toml` to your ignored `dev/` directory. Set:

```toml
[downloads]
enabled = true
directory = "dev/downloads"

[downloads.youtube]
enabled = true
```

Unspecified settings retain their defaults; the complete example documents
all resource limits. Restart after changing configuration. Downloads are
explicitly disabled by default.

The managed directory is exclusively owned by Shprotify, created with mode
0700 and must not overlap `library.directories`. An existing root must already
be private. Paths containing symlinks or parent traversal are rejected. Do not
add this directory to external scan roots or modify its contents while the
server is running. Existing external collections remain read-only.

Keep `downloads.directory` configured when disabling either feature switch:
previously imported files remain streamable and publication recovery still
runs. Queued work waits while acquisition is disabled; it can still be
cancelled. Removing the directory configuration makes managed files
unavailable for streaming; pending publication intents remain retained for a
later restart with the correct directory.

## Member workflow

Use an ordinary personal key or password session as `$TOKEN`; no administrator
credential is needed. All acquisition routes require authentication and use
`Cache-Control: no-store` for successful responses. Private searches/jobs are
visible only to their owner, including when another account is an admin.
The imported catalog recording and audio are shared with the instance.

Discover the configured provider (availability is not a live health probe):

```sh
curl -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:3000/api/v1/downloads/providers
```

Start a search for a visible recording candidate from Discovery:

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"kind":"candidate","id":42}' \
  http://127.0.0.1:3000/api/v1/me/downloads/searches
```

Alternatively use `{"kind":"recording","id":7}` for an existing shared
recording, or `{"kind":"query","title":"Song","artists":["Artist"]}`
without creating catalog rows yet. Release/playlist candidates are rejected;
choose one recording from their contents instead. Inputs reject unknown
fields. No arbitrary URL, executable options or destination path can be sent.

The response is `202 Accepted` with a durable job and a `Location` header:

```json
{
  "id": 12,
  "operation": "search",
  "status": "queued",
  "video_id": null,
  "retry_of": null,
  "cancel_requested": false,
  "downloaded_bytes": 0,
  "total_bytes": null,
  "recording_id": null,
  "file_id": null,
  "created_at": 1795000000,
  "finished_at": null,
  "message": null
}
```

Poll `GET /api/v1/me/downloads/12`. When completed, fetch
`GET /api/v1/me/downloads/12/sources`: up to five source selections with an
`id`, stable `video_id`, public `url`, title, channel and optional duration.
Live/upcoming streams and known overlong videos are excluded. Empty results
are a successful search, not a catalog mutation. Titles and channels come
from YouTube and do not prove recording identity.

Submit a selected source ID from your own search, within one hour:

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"source_id":25}' \
  http://127.0.0.1:3000/api/v1/me/downloads
```

Poll the returned job through `queued`, `running`, `verifying`, `importing`
and `completed`. `downloaded_bytes` is approximate observed scratch usage;
remuxing may temporarily count input and output, so it is not monotonic.
`total_bytes` is currently unknown/null; clients should show status and bytes,
not invent a percentage. On completion, use the returned `recording_id` and
`file_id` with the normal authenticated streaming endpoint. Downloading does
not create a playback-history event.

The best available audio-only AAC/Opus source is selected. FFmpeg may extract
or remux the container, preserving the codec; Shprotify does not request MP3
conversion, lossless conversion or video downloads. Actual format, encoded
size, duration, sample rate and channels are probed before import; source
bytes are hashed with SHA-256. A material duration discrepancy from the
selected source is rejected. This validates the container/properties, not
full decoding of every sample or acoustic confirmation of song identity.

Desired catalog title/credits come from the user's requested track, not the
video title or embedded tags. Original embedded tags remain available through
the existing source-tags API. The selection does not automatically confirm
MusicBrainz IDs or create an album/tracklist. Discovery links are marked
`download_selection`. Existing catalog metadata remains unchanged.

Exact verified source/content duplicates reuse a readable file and recording;
a repeated source can finish without another provider invocation. A source
already associated with a different explicit Recording is a conflict rather
than an automatic merge. A registered duplicate that is missing, changed or
outside current allowed roots fails safely and requires administrator repair.

An imported Discovery candidate gains live local availability. Retained likes
and dislikes are reconciled into native reactions/favorites only when a newer
explicit native decision does not exist. Explicit clears/reaffirmations are
remembered too. Wishlist intent remains retained; availability changes
independently. Reactions belong to users, not the common source file.

## Cancel, retry and inspect failures

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:3000/api/v1/me/downloads/13/cancel

curl -X POST -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:3000/api/v1/me/downloads/13/retry
```

Cancellation is cooperative and polled while provider work runs. It kills
the process group, including FFmpeg/Deno children. Queued cancellation is
immediate; verification is awaited before acknowledging cancellation so a
blocking parser cannot outlive its file/resource permit. Once durable import
starts, cancellation returns `409`; publication/registration finish together.
Cancelling a terminal job is idempotent.

Retry creates a new job with `retry_of`; only failed/cancelled/interrupted jobs
can be retried. Ownership, source intent and queue limits are rechecked. A
changed target requires a fresh source search. There are no endless automatic
job retries; yt-dlp itself has a small bounded network-retry allowance.

`GET /api/v1/me/downloads?limit=50&offset=0` pages owned history, newest first.
Limits are 1–100, offset nonnegative, unknown query fields rejected. Errors
use the existing JSON envelope. Disabled-provider requests return 422 before
queue admission. Full queues, duplicate work, stale intent, source cooldown,
and import-boundary conflicts return 409. Other accounts' job/source IDs
return 404. Provider failures appear as safe job messages, not HTTP 500s on
normal polling, and never include raw subprocess diagnostics or credentials.

## Resource limits and restart behavior

Defaults allow two concurrent jobs, at most twenty queued/active jobs across
the instance and three for one account. Searches share these slots, have a
five-second per-account cooldown and a 60-second timeout. Downloads have a
ten-minute provider deadline, 256 MiB encoded-file limit and 30-minute duration
limit. Admin jobs obey the same acquisition quotas. Permission to acquire is
separate from the administrator role for catalog editing/deletion.

The server also monitors scratch usage independently of yt-dlp. Remuxing can
need twice the file limit; polling can briefly overshoot a disk threshold.
These are application bounds, not filesystem quotas or an OS process sandbox.
Keep adequate free space or apply deployment-level quotas when needed. The
managed library grows as users add music; its total lifetime size is not capped
by the per-job limit. Parser work runs on blocking threads and is awaited.

Final paths are `objects/<sha256>.<detected-format>` below the private managed
root. They cannot collide through source titles. Publication never replaces an
existing object, and its bytes are verified again before the catalog commit.
A SQLite intent is written before publication; the catalog/file/job/provenance
updates commit together afterward.

On shutdown, queued intent stays durable, provider processes stop and active
unjournaled work becomes interrupted. Critical imports finish before the DB
closes. On restart, queued work resumes when enabled; interrupted downloads
require an explicit retry. Successful old jobs are never replayed.

Publication intents recover before HTTP startup, including a crash before or
after the final filesystem link. Corrupt artifacts fail only their job and
unreferenced failed objects are removed. A transient database failure retains
the journal for recovery after restart; another download of that source/target
is blocked until recovery resolves it. SQL/storage initialization failures
still require operator attention, as with other database startup failures.
Scratch directories are cleaned after terminal jobs and during recovery;
external audio files are never renamed, overwritten or deleted.

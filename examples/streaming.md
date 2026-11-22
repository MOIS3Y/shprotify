# Authenticated audio streaming

Choose a registered physical file using
`GET /api/v1/recordings/{id}/files`, then request:

```text
GET /api/v1/recordings/{id}/files/{file_id}/stream
HEAD /api/v1/recordings/{id}/files/{file_id}/stream
```

Both methods require `Authorization: Bearer <credential>`. An ordinary member
or administrator may use a personal key or an unexpired password session.
There is no query-string token transport. Missing, revoked or expired
credentials return JSON 401 with a Bearer challenge before checking file IDs,
ranges or the filesystem. HTTPS is required outside loopback.

```sh
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -o dev/track.flac \
  http://127.0.0.1:3000/api/v1/recordings/1/files/1/stream
curl -I -H "Authorization: Bearer $PERSONAL_API_KEY" \
  http://127.0.0.1:3000/api/v1/recordings/1/files/1/stream
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Range: bytes=1024-2047' -o dev/track-part.bin \
  http://127.0.0.1:3000/api/v1/recordings/1/files/1/stream
```

The response contains the original file bytes. There is no transcoding,
decoding, tag rewriting, automatic file selection, or recording merge. The
catalog recording/file association must match both URL identifiers.

## HTTP behavior

- Full GET: 200, `Content-Length` measured from the open physical file,
  `Accept-Ranges: bytes`, and the original bytes.
- Single satisfiable byte range: 206, selected `Content-Length` and
  `Content-Range: bytes first-last/size`.
- Invalid or unsatisfiable single bytes range: 416, the shared JSON error
  with code `range_not_satisfiable`, and `Content-Range: bytes */size`.
- HEAD: full-file headers and no body. Range is ignored for HEAD.
- Unknown recording/file, wrong parent, missing/inaccessible file,
  nonregular file, disallowed path or symlink: 404 without filesystem details.
- Malformed or nonpositive identifiers: 400. Unexpected storage/I/O failures
  before sending the response use the safe JSON 500 envelope.

Ranges support `bytes=first-last`, `bytes=first-`, and `bytes=-count`.
Endpoints beyond EOF are clipped; an oversized suffix selects the whole file
with status 206. Zero-length suffixes and starts at/beyond EOF return 416.
An empty file has a valid full 200 response of zero bytes; requesting bytes
from it returns 416 with `Content-Range: bytes */0`.

Multipart ranges, multiple Range header lines and unknown range units are
ignored and receive the full 200 response. ETag and Last-Modified validators
are not issued at this stage; requests containing If-Range receive the full
representation. Content-Encoding compression is not applied, so byte offsets
always refer to the original file. Successful responses have
`Cache-Control: private, no-store` and `X-Content-Type-Options: nosniff`.

Content-Type follows the catalog format: MP3 `audio/mpeg`, MP4 `audio/mp4`,
FLAC `audio/flac`, WAV `audio/wav`, AAC `audio/aac`, and Ogg Opus/Vorbis/Speex
`audio/ogg`. Other format labels use `application/octet-stream`. The server
does not probe or reindex media while streaming. OpenAPI documents binary
responses and the Range headers beside the handler.

## Filesystem policy

A registered path alone does not grant access. It must be a normal descendant
of one of the currently configured `library.directories`. An empty roots list
permits catalog metadata operations but no streaming. Removing a root and
restarting makes its files unavailable for new requests without deleting any
catalog entries.

Startup resolves configured root aliases to canonical directories. Streaming
opens every path component relative to directory handles with nofollow,
rejecting nested symlinks and parent traversal. Only regular files are served;
FIFO opens are nonblocking and immediately rejected. A root replaced with a
symlink is rejected until the operator explicitly reconfigures/restarts.
Hard links are regular directory entries and follow the same root policy.

Filesystem opening and metadata work run off the async workers. The response
reads incrementally with a 64 KiB initial buffer and bounds reads to the
measured size/range. It does not load the entire track or hold a SQL
transaction while transferring. Replacing the pathname after opening does not
redirect the open handle. The response does not include filenames or paths.

Contents are not snapshotted: in-place source writes during transfer can alter
the remaining bytes. Appended bytes beyond the measured size are excluded;
truncation or an I/O failure after headers can result in an incomplete HTTP
transfer, not a replacement JSON error. Rescanning does not mutate files, so
normal catalog scans can coexist with streaming. Stream reads do not update
catalog metadata, reactions or listening history. Revocation applies to new
requests and does not cancel an already accepted transfer.

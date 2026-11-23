# Catalog and playback

To play a track, find its recording in the catalog, pick an audio file
and pass the stream to your player. You'll need a token for these examples.
[Get a token and try a stream](./overview.md)

## Find music in the catalog

| What to open | Request |
| --- | --- |
| Artists | `GET /api/v1/artists` |
| An artist's albums | `GET /api/v1/artists/{id}/releases` |
| Album tracks | `GET /api/v1/releases/{id}/tracks` |
| Recordings | `GET /api/v1/recordings` |
| Files for a recording | `GET /api/v1/recordings/{id}/files` |
| Catalog search | `GET /api/v1/search?q=...&limit=20` |

Search returns a `kind`, `id` and `title` for each result. Use
`kind` to choose the artist, album or recording screen.

Use `limit` and `offset` for subsequent pages. The recordings list includes
`has_more` and `total`. Search and file lists return `items`, `limit`
and `offset`. A page shorter than the requested size marks the end.
See the [API reference](../reference/api.md) for each response shape.

## Distinguish recordings, album positions and files

| What it is | Where it matters |
| --- | --- |
| Recording (`recording_id`) | The performance itself. Favorites, queue entries and history refer to it. |
| Album position | An occurrence of a recording at a particular disc and track position. |
| Audio file (`file_id`) | One copy of the recording in a particular format. Streams use both IDs. |

A recording can have several files or none yet. Show when there is no
audio to play and choose a file in a format your decoder supports.
The server streams audio in its original format.

Use batch requests to load the visible rows. For recordings, use
`GET /api/v1/recordings/batch?ids=12,34,56`. For reactions, use
`POST /api/v1/me/recordings/reactions` with
`{"recording_ids":[12,34,56]}`. Read `batch_limit` from capabilities.
IDs and versions are 64-bit integers. In JavaScript, account for the
precision limit of an ordinary `Number`.

## Play in a native client

For recording 12 and file 7, give your audio engine the URL
`/api/v1/recordings/12/files/7/stream` on the server and the header
`Authorization: Bearer TOKEN`. Include the header on seek requests too.

| Request | Server response |
| --- | --- |
| `HEAD` | Headers without audio. |
| `GET` with one satisfiable byte `Range` | `206 Partial Content` and `Content-Range`. |
| A range outside the file | `416 Range Not Satisfiable`. |
| A request for multiple ranges | The full file with `200 OK`. |

## Play in a browser {#browser-playback}

For an `<audio>` element, request a temporary link with
`POST /api/v1/recordings/{id}/files/{file_id}/playback-ticket` and
the `Authorization` header. The response includes `stream_url` and
`expires_at` in Unix seconds. Resolve the URL relative to the server
address, then assign it to `audio.src`.

The link applies to one file. After expiry, request a new link before
the next audio request. When you swap `src` during playback, remember
the position and restore it once metadata loads. Signing out or
revoking the original credential invalidates the link too.

> [!IMPORTANT]
> Use the returned `stream_url` as the audio URL. Send the account token
> only in the `Authorization` header.

If the player page uses a different domain, ask the administrator to add
that address to the server's [CORS settings](../guide/proxy.md#if-a-web-player-uses-another-domain).

## Artwork and offline playback

- **Artwork.** Use `GET /api/v1/artwork/{kind}/{id}`. Send a token for a
  private catalog. Save the `ETag` and send it with `If-None-Match`.
  A `304` means you can keep your cached image.
- **Offline files.** Store the original audio and metadata locally. Audio
  streams have no `ETag` or `Last-Modified`. An `If-Range` request
  returns the full file. Restart a partial download if you can't verify
  it is still the same file.

Once playback works, add a [queue and listening history](./collections.md).

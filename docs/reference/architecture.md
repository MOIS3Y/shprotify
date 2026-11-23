# Architecture

Shprotify is one Rust HTTP server backed by SQLite. The server stores
the shared catalog, serves audio files and manages personal collections.
Any player can connect through the [HTTP API](./api.md). The player
controls playback and decides how to present the library.

## Follow a request

When a player lists recordings, `api` checks the parameters and access
rights, then `db` reads the catalog from SQLite. To play a recording,
the client chooses one of its files. The server opens it through `audio`
and serves its original bytes, including byte ranges for seeking.

| What you want to change | Where to look |
| --- | --- |
| Startup, settings, logging or local commands | `main`, `app`, `cli`, `config`, `logging` |
| A route, permission check or response schema | `api` and `auth` |
| Queries, transactions and storage | `db` |
| File access, tags and catalog rules | `library` and `audio` |
| Playlists, listening history and recommendations | `playlists`, `audio` and `discovery` |
| An external service, import or schedule | `providers`, `downloads`, `jobs` and `automation` |

`main.rs` hands control to the application. HTTP handlers own the
contract and permission checks. SQL operations stay in `db`. Calls to
providers and external tools run outside database transactions.

## Scans and other background jobs

Scanning, provider lookups and downloads can take time. The server
accepts the request, saves a job and returns a status URL. A client can
poll that URL to show the result or error.

A download fetches and validates the audio before importing it into the
catalog. An upstream error stays with the job. Schedules run the same
operations through an interval dispatcher. Administrators enable the
integrations they need. Catalog and playback tests use local data.

## What lives in SQLite and on disk

Artists, releases, recordings and files belong to the shared catalog.
A release links recordings to disc and track positions. A recording
can have multiple audio files. Favorites and history refer to the
recording, so replacing a file keeps that connection.

Personal state belongs to an account. Publishing a playlist lets other
users read it while the owner keeps editing it. Playlists, queues, feeds
and schedules use versions to guard against stale edits. A history event
ID lets a client retry after a connection failure without making a duplicate.

SQLite stores the catalog, metadata, cover art and personal state.
Music stays in the folders the operator connected. Managed downloads are
stored separately, using a hash of the file contents.
[Database schema](./database.md)

## How the server and site are built

Nix and crane build and check the server. The Docker image uses the same
package. Documentation builds separately as a static site with OpenAPI and
the SQLite schema. Its tools are only needed to build the site.
[Documentation and references](./documentation.md)

Shprotify exposes a versioned API and a list of server capabilities.
Players are separate applications and work with the original audio formats.

# Personal discovery and library growth

Discovery works with local recordings and candidates that have no local audio
or MusicBrainz ID. It does not create shared catalog entries, modify source
files, download audio or submit listening history to external services.
All routes below require a current personal Bearer key or password session.
Feeds, jobs, preferences, feedback and acquisition wishes belong to the account;
an administrator has no implicit access to another user's discovery.
Successful responses use `Cache-Control: private, no-store`.

## Profile and onboarding

`GET /api/v1/me/discovery/profile` returns explicit `preferences` plus inferred
`artists` and `tracks`. Preferences start at version zero without a database
write. Follow existing local artists and optionally choose topical tags:

```sh
curl -X PUT "$BASE/api/v1/me/discovery/preferences" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"version":0,"artist_ids":[3,8],"tags":["русский рок","electronic"]}'
```

Settings replace the whole artist/tag selection. Up to 20 distinct existing
artist IDs and 20 case-insensitively distinct tags (1–100 Unicode characters)
are allowed. Unknown fields and control characters are rejected. Actual changes
advance the version; identical current-version writes retain it; stale writes
receive `409`.

The initial ranking algorithm is explainable and does not require audio models,
embeddings or a vector database. It uses only this account's signals:

- A recording like contributes 8; a dislike excludes that recording's signals.
- Full plays in 30 days contribute 1 each, in 90 days 0.5, and in one year 0.1;
  their aggregate contribution is capped at 8 per recording. Older plays and
  short previews do not contribute. A preview never becomes a dislike.
- Credit weight is shared across the recording's credited artists; explicit
  artist follows add 12. Artist affinities are bounded, and top seed lists are
  limited to 20 artists and ten distinct tracks.
- Likes on unresolved external candidates also supply seeds. Matched recordings
  use their current local assessment; cached external likes cannot override a
  local dislike. A recording dislike does not blacklist its entire artist.

Provider queries receive only selected seed names/IDs/tags, not the user's
identity, complete history or raw audio. The Last.fm application key is an
operator setting, independent of user accounts and password sessions.

## Generate a collection

```sh
curl -X POST "$BASE/api/v1/me/discovery/feeds" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"kind":"personal"}'
```

Returns `202` and an owned refresh job with a
`Location: /api/v1/me/discovery/jobs/{id}` header. Poll that URL for
`running`, `completed`, `failed` or `interrupted`. The job also contains
`feed_id`, used for reading the collection and its items.

| Request body | Purpose |
| --- | --- |
| `{"kind":"personal"}` | Local discoveries plus enabled external sources |
| `{"kind":"rediscover"}` | Liked local tracks not played fully in 30 days |
| `{"kind":"chart"}` | Last.fm global chart |
| `{"kind":"country","country":"Russian Federation"}` | Country chart; country is not a track-language filter |
| `{"kind":"tag","tag":"russian rock"}` | Last.fm thematic tracks |
| `{"kind":"similar_track","artist":"Кино","track":"Группа крови"}` | Similar recordings |
| `{"kind":"similar_artist","artist":"Кино"}` | Tracks from up to two similar artists |
| `{"kind":"search","query":"Звезда","artist":"Кино"}` | External track search; artist is optional |
| `{"kind":"new_releases"}` | Editions released in 90 days by personal artist MBID seeds |
| `{"kind":"playlist_search","query":"russian rock"}` | Search public ListenBrainz playlist titles/descriptions |
| `{"kind":"playlist","playlist_id":"<UUID>"}` | Open a public ListenBrainz playlist |

Search text is plain text, not SQL or MusicBrainz query syntax. Playlist search
requires at least three characters. UUIDs are validated before building paths;
arbitrary URLs are not accepted as upstream endpoints.

`personal` works without any external integration. With Last.fm enabled it
combines up to three track seeds, one similar-artist search, the first preferred
tag, and a chart exploration source. With ListenBrainz enabled it can also
suggest public playlists using the first tag or strongest artist name.
Individual Shprotify users do not need external accounts or scrobbling
for these queries; the operator supplies the application key.

Local candidate generation considers a bounded pool of 1000 recordings, favoring
related artists and rotating other choices daily. Personal local discoveries
exclude likes, dislikes and recordings played fully within seven days;
`rediscover` intentionally selects older favorites. Before persistence, at most
300 distinct candidates are retained. Read-time ranking adds bounded artist
affinity to source relevance, supplies reasons, and moves fourth/subsequent
appearances of an artist after more diverse choices. Scores are local ordering
values, not probabilities, audio similarity measurements or global ratings.
Cold-start profiles can use explicit follows/tags, a chart, or local exploration.

`new_releases` requires confirmed artist MusicBrainz IDs and searches up to five
artist seeds. An edition released recently can be a reissue; this is not a claim
that every recording on it is newly created. It recommends releases as units,
without importing their tracklists into the catalog.

## Read, filter and retain

| Method | Path after `/api/v1` | Purpose |
| --- | --- | --- |
| GET | `/me/discovery/feeds` | Owned collection definitions |
| GET | `/me/discovery/feeds/{id}` | Title, request, source URL, warnings and generation version/time |
| GET | `/me/discovery/feeds/{id}/items` | Ranked or source-ordered candidates |
| POST | `/me/discovery/feeds/{id}/refresh` | Explicitly refresh the saved request |
| GET | `/me/discovery/jobs` | Owned refresh history |
| GET | `/me/discovery/jobs/{id}` | Owned job status |
| GET | `/me/discovery/candidates/{id}` | Previously seen or retained candidate |
| PUT | `/me/discovery/candidates/{id}/feedback` | Versioned complete personal decision |
| GET | `/me/discovery/feedback` | Retained decisions, including removed source items |
| GET | `/me/discovery/wishlist` | Candidates marked “want to add” |
| POST | `/me/discovery/feeds/{id}/playlist` | Copy eligible local recordings into a private playlist |

Lists accept `limit=1..100` (default 50), `offset>=0`. Item pages additionally
accept `missing_only=true` and `include_hidden=true`. Filters are applied before
pagination. Personal, rediscovery and recent-release feeds use local ranking;
charts, search results and imported playlists keep the original source order.
Discovery deduplicates repeated source identities, keeping their first position.
Large public playlists import the first 300 occurrences and report truncation;
upstream response bodies themselves are capped at 2 MiB.

A candidate contains stable `id`, `candidate` metadata, optional `local_id` and
`match_method`, registered-audio status, effective `reaction`, versioned
`feedback`, score and reasons. Its kind can be `recording`, `release` or
`playlist`. A playlist-search candidate's `external_id` is the UUID to use in
a new `playlist` request. Its URL opens the source information page; it is not
an audio preview or a download location.

Verified MBIDs are preferred for matching. Without an MBID, one exact title
and complete ordered artist-credit match can identify a local entity. Ambiguous
or unmatched results keep `local_id: null`; this does not prove that no copy
exists under another spelling. Cyrillic metadata is preserved. Case-insensitive
fallback is conservative and does not perform transliteration or fuzzy merging.
Provider IDs remain usable without MBIDs. Cross-provider canonical identities
are combined by MBID; later conflicting/ambiguous provider identity changes
never silently transfer an old personal decision to another candidate.

`missing_only` excludes matched recordings with registered audio; for releases
it excludes those for which every known local release track has a file. A
partial local tracklist cannot prove that a complete external album is present.
Registry presence does not prove current filesystem availability: streaming
still verifies roots/files, and downloading must recheck availability.

Feed version describes source generation/membership. Local catalog corrections,
likes/hides and the live profile can change the projection between pages without
changing that version. Refresh/reload if collecting a stable full result is
important; there is no immutable ranked cursor in this stage.

## Feedback, favorites and acquisition wishes

```sh
curl -X PUT "$BASE/api/v1/me/discovery/candidates/42/feedback" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"version":0,"reaction":"like","hidden":false,"wanted":true}'
```

`reaction` is `like`, `dislike` or null. `hidden` and `wanted` are required booleans,
independent of assessment. Version zero means no previous decision. Updates
replace all fields, retain current versions for no-ops and reject stale versions
with `409`. Hidden/disliked candidates are suppressed by default; use
`include_hidden=true` to review them. Personal discovery also suppresses likes;
rediscovery is the explicit way to revisit favorites.

For a currently matched recording, changing assessment also updates its existing
Recording reaction atomically, so a like appears in `/me/favorites`. The card's
`reaction` is the current local assessment; `feedback.reaction` retains the last
discovery decision separately and can differ after a direct Recording edit.
For an unresolved candidate, a like is retained in discovery without creating
a shared catalog Recording. Wishlist includes explicit `wanted` candidates,
even hidden ones or candidates no longer in a refreshed source collection.
`GET /me/discovery/feedback?reaction=like` lists retained Discovery likes;
`hidden=true` helps review/undo previous hides even after source refreshes.
These filters refer to the last Discovery decision, while each returned card
also includes the current effective local reaction.
The acquisition stage will resolve/import selected candidates and reconcile
these decisions with their final local Recording identities.

To create an ordinary playlist from matched recordings:

```sh
curl -X POST "$BASE/api/v1/me/discovery/feeds/7/playlist" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"version":1,"name":"Discovered locally"}'
```

The `201` response contains a private playlist and `included`/`skipped` counts.
Only currently matched recordings that are not hidden/disliked are copied in
source order. Whole releases, external tracks and playlist cards remain in
discovery. No eligible recordings means `422`; a stale feed version means `409`.
Future source refreshes do not modify the exported ordinary playlist.

## Configuration and failure behavior

See `[discovery]` in the complete `examples/shprotify.toml`. External adapters
are disabled by default. Set `enabled`, a meaningful `user_agent`, and the
Last.fm application key for Last.fm; enable the two other adapters independently.
This does not enable Last.fm as a catalog metadata provider or change existing
`[metadata]` settings. Blank Last.fm key leaves local discovery usable.

Normalized public responses are cached for the configured TTL (60–86400 seconds),
shared without user feedback, account IDs, raw API responses or API credentials.
Expired entries are removed on cache writes. Feed definitions and selected
metadata remain persistent independently of this response cache.
One Discovery refresh runs per instance; per-account admission also has a
configurable cooldown (default 60 seconds). Busy/cooling-down writes return
`409`. External request starts share one-second pacing with metadata adapters;
HTTP connection/total deadlines are 5/30 seconds, overall job deadline 120.

An explicit source request that fails leaves the last successful feed intact.
Personal generation can retain local/available results and report safe warnings
if optional sources fail. Completion and membership replacement are atomic.
Accepted work continues after its admission request finishes; graceful shutdown
waits for it. Crash recovery marks old running jobs `interrupted`, without
replaying external requests automatically. Generation is explicit; schedules,
cache/job retention controls and automatic refreshes belong to stage 7.

This stage provides the bridge to acquisition through stable candidate IDs,
source identities and the wishlist. Preview audio and downloadable sources are
provider-dependent and are not supplied by the recommendation APIs themselves.
Audio embeddings, Last.fm account synchronization and automatic downloading
are not part of this implementation.

Provider contracts: [Last.fm API](https://www.last.fm/api),
[ListenBrainz playlists](https://listenbrainz.readthedocs.io/en/latest/users/api/playlist.html),
[MusicBrainz search](https://musicbrainz.org/doc/MusicBrainz_API/Search).

# Discovery and downloads

Add recommendations once [playback](./playback.md) and
[personal collections](./collections.md) work. A feed helps listeners
find music. The player can open it when a matching audio file is in the library.

## Create a feed and read its results

Send one of the requests below to `POST /api/v1/me/discovery/feeds`.
A personal feed can use the local library. A tag feed needs Last.fm enabled.
[Sources and setup](../guide/discovery.md)

::: code-group
```json [Personal feed]
{"kind":"personal"}
```
```json [Last.fm tag]
{"kind":"tag","tag":"jazz"}
```
:::

1. After `202 Accepted`, poll the URL from `Location`. In the response
   body, `id` is the job ID and `feed_id` identifies the feed.
2. Once the job is `completed`, read
   `GET /api/v1/me/discovery/feeds/{id}/items` with the feed ID.
3. For a later refresh, send `POST /api/v1/me/discovery/feeds/{id}/refresh`.
   Follow the new job's status. You can keep showing the previous results
   if it fails.

## Explain why a suggestion appeared {#display-recommendations}

| Field | How to use it |
| --- | --- |
| `candidate.provider` and `candidate.url` | Show the source and, when there's a URL, offer to open its page. |
| `reasons` | Display explanations from the server, such as "From the shared library" or "Matches your interest in an artist". Use structured fields to decide which actions to offer. |
| `id` | Use it for actions on the suggestion, such as adding it to the wishlist. |
| `candidate.kind` and `local_id` | Use the kind to choose the local recording or release screen. An unmatched result has a `null` local ID. |
| `has_registered_audio` | For a recording, this means a file is registered. The server checks the actual file when streaming. |

Keep the order returned by the server. The `score` helps rank results.
It isn't a percentage or a track rating.
[How recommendations are chosen](../guide/discovery.md#how-recommendations-work)

For a recording with a `local_id` and `has_registered_audio: true`,
list its files and offer playback. For a matched release, you can show
its tracks. If the audio is missing, offer to save the suggestion.

## Save or hide a suggestion

Read `GET /api/v1/me/discovery/candidates/{id}` first. Send the current
feedback with your changes to
`PUT /api/v1/me/discovery/candidates/{id}/feedback`. This example saves
a suggestion to the wishlist when it hasn't been rated yet:

```json
{"version":0,"reaction":null,"hidden":false,"wanted":true}
```

Use the version in `feedback.version`. Set `hidden: true` to hide it or
`wanted: true` to keep it in `GET /api/v1/me/discovery/wishlist`.
A reaction to a suggestion is separate from a like on the library recording.
Use `feedback` when editing the suggestion. The top-level `reaction`
shows the reaction the server uses for the feed.

To create a regular playlist, send
`POST /api/v1/me/discovery/feeds/{id}/playlist` with the current feed version:

```json
{"version":1,"name":"Jazz discoveries"}
```

Check `included` and `skipped` for the number of recordings added or
skipped. The playlist contains eligible catalog recordings and stays the
same when the feed refreshes.

## Find audio for a track

Before offering downloads, check `downloads_enabled` in capabilities and
the provider's `enabled` flag from `GET /api/v1/downloads/providers`.
An enabled provider can still be temporarily unavailable.

Send one of these requests to `POST /api/v1/me/downloads/searches`:

::: code-group
```json [From a suggestion]
{"kind":"candidate","id":42}
```
```json [Catalog recording]
{"kind":"recording","id":12}
```
```json [By title]
{"kind":"query","title":"Track title","artists":["Artist name"]}
```
:::

Use actual IDs or search terms. A discovery candidate must refer to one
recording. Once the search job is `completed`, read
`GET /api/v1/me/downloads/{id}/sources` with the job ID. Show the
video title, channel and duration so the listener can choose a version.

## Download the selected result

Send `POST /api/v1/me/downloads` with the ID of the selected search result:

```json
{"source_id":17}
```

Use a source from this account's recent successful search. After
acceptance, poll `GET /api/v1/me/downloads/{id}` with the **new** job ID.
Show download, verification and import as separate stages. The total size
can be unknown, in which case `total_bytes` is `null`.

After `completed`, use the returned `recording_id` and `file_id` to
[play the file](./playback.md). On failure, show the job's `message`.
Use `POST /api/v1/me/downloads/{id}/cancel` or
`POST /api/v1/me/downloads/{id}/retry` when the job's state allows it.

[Administrator setup for YouTube downloads](../guide/downloads.md)

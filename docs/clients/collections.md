# Favorites, playlists, queue and history

These features belong to the signed-in account. Send its bearer token
and use recording IDs from the catalog. In these examples, `12` and
`34` are recordings you've already found. [First API requests](./overview.md)

## Add a favorite

Send `PUT /api/v1/me/recordings/12/reaction`:

```json
{"reaction":"like"}
```

The track will appear in `GET /api/v1/me/favorites`. To dislike it,
send `{"reaction":"dislike"}`. To clear the reaction, use
`DELETE /api/v1/me/recordings/12/reaction`. Both remove it from favorites.
The server derives favorites from likes.

## Create a playlist and set the track order

1. Send `POST /api/v1/me/playlists`:

   ```json
   {"name":"Evening","description":""}
   ```

2. Take the `id` and `version` from the response. Send
   `PUT /api/v1/me/playlists/{id}/tracks` with that version:

   ```json
   {"version":1,"recording_ids":[12,34,12]}
   ```

The array replaces the **entire** order. To add one track to an
existing playlist, first read `GET /api/v1/me/playlists/{id}/tracks`,
add the ID to your list, then send the whole list. Repeated IDs are
allowed. An empty array clears the playlist.

> [!TIP]
> When you read several pages, check that they all have the same
> `version`. On `409`, reload the playlist and reconcile your changes.
> After a successful write, use the new version from the response.

### Share the playlist

Send `PUT /api/v1/me/playlists/{id}/visibility` with the current version:

```json
{"version":2,"is_public":true}
```

Other signed-in users can now find it through `GET /api/v1/playlists`.
The owner can edit it. To make it private again, send `is_public: false`.

## Save the queue and playback position

Read `GET /api/v1/me/queue`. To replace the order and save the current
selection, send `PUT /api/v1/me/queue`:

```json
{"version":0,"recording_ids":[12,34,12],"current_position":3,"position_ms":0}
```

Use the version returned by GET. Zero is the initial empty queue.
Here `current_position: 3` selects the **second** occurrence of recording
12 because positions start at one. For no selection, send
`current_position: null` and `position_ms: 0`.

To save selection or progress after setting the order, send
`PUT /api/v1/me/queue/playback` with the current `version`,
`current_position` and `position_ms`. Read the maximum size from
`collection_limit` in capabilities.

The client starts playback, advances tracks and controls repeat.
It records listening history separately.

## Record a listen

When a track ends or the listener skips it, send `POST /api/v1/me/history`:

```json
{
  "event_id":"device1-play-0001",
  "recording_id":12,
  "started_at":1780000000,
  "listened_ms":95000,
  "duration_ms":180000
}
```

Use the actual start in Unix seconds and the time listened in milliseconds.
Leave out pauses and seeks, and cap listening time at the known duration.
If duration is unknown, omit `duration_ms` or send `null`.

| Response | What it means |
| --- | --- |
| `201` | A new event was saved. |
| `200` | The same report had already been saved. |
| `409` | That `event_id` already belongs to different data. |

Create a new `event_id` for each play. If the connection fails, keep the
report locally and retry with the same ID and payload.

The server returns `full` after half the track or four minutes, whichever
is shorter. If duration is unknown, the threshold is four minutes.
Shorter listens are `short`. Use `GET /api/v1/me/history` for the history screen.

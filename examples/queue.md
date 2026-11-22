# Personal playback queue

All routes require a current personal key or password session in
`Authorization: Bearer <token>`. Each account has one independent queue;
admins access their own queue through the same routes. Successful responses
use `Cache-Control: private, no-store`.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/v1/me/queue` | Read the complete queue and playback position |
| PUT | `/api/v1/me/queue` | Replace order and selection atomically |
| PUT | `/api/v1/me/queue/playback` | Change selection/offset, preserving order |

An untouched account returns this snapshot without creating a database row:

```json
{"version":0,"current_position":null,"position_ms":0,"items":[]}
```

Replace the queue, selecting the second occurrence of recording 12:

```sh
curl -X PUT "$BASE/api/v1/me/queue" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"version":0,"recording_ids":[12,34,12],
       "current_position":3,"position_ms":15000}'
```

The response includes the resulting version and the full ordered items:

```json
{
  "version":1,
  "current_position":3,
  "position_ms":15000,
  "items":[
    {"position":1,"recording_id":12,"title":"First recording"},
    {"position":2,"recording_id":34,"title":"Another recording"},
    {"position":3,"recording_id":12,"title":"First recording"}
  ]
}
```

To persist a seek or advance to another occurrence, submit the latest version:

```sh
curl -X PUT "$BASE/api/v1/me/queue/playback" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"version":1,"current_position":2,"position_ms":0}'
```

Both mutations share a version, including changes to the selection/offset.
Each actual change increments it once. Identical requests using the current
version are no-ops. Stale versions return `409`, even if their payload matches
current state. After a conflict or uncertain response, GET the latest snapshot
and reconcile explicitly; do not blindly replay an old replacement.
Concurrent first writes at version zero also follow this rule.

Order replacement is the initial editing contract: append, remove, reorder
and clear by sending the desired complete `recording_ids` array. At most
10000 occurrences are accepted; duplicates and an empty order are supported.
Every ID must refer to an existing Recording, without requiring a local file.
Invalid references reject the whole transaction with `422`. The selected
occurrence is explicitly relative to the replacement order; the server does
not guess which duplicate should remain selected after a reorder.

`current_position` is one-based, or null/omitted for no selection. Clearing
selection requires `position_ms: 0`. Empty queues cannot have a selection.
`position_ms` is a nonnegative media offset in milliseconds, independent of
listening time. There is no inferred duration limit because different files
may have different durations. `version`, `position_ms` and (for replacements)
`recording_ids` are required. Unknown fields are rejected; the request body
limit is 256 KiB. GET and successful mutations return a complete bounded
snapshot, with live catalog titles. Titles can change without a queue version
change; versions protect queue order and position, not catalog metadata.

Queues persist across server restarts and are separate from playlists,
favorites and playback history. Reading or updating one does not open files,
start/stop playback, advance automatically, or report listening events.
Clients control playback and submit finished listening facts to `/me/history`.
Position updates are checkpoints, not a server-side playback clock or device
control protocol. There is no device arbitration, repeat/shuffle mode,
automatic playlist synchronization or push notification in this stage.

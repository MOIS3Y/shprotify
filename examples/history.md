# Personal listening history

Clients report one finished or stopped playback to
`POST /api/v1/me/history`. Fetch the private timeline with
`GET /api/v1/me/history?limit=50&offset=0`, or one accepted event with
`GET /api/v1/me/history/{id}`. All three routes require an active personal key
or password session. Administrators also see only their own history.

## Reporting a playback

```json
{
  "event_id": "phone-play-8e48a77f-74dc-4cae-878d-d34873e554f2",
  "recording_id": 1,
  "started_at": 1700000000,
  "listened_ms": 90000,
  "duration_ms": 180000
}
```

`event_id` is an opaque, case-sensitive client key: 1–128 ASCII letters,
digits, `.`, `_`, `-`. Generate a new key for every playback attempt, including
replaying the same recording. A random UUID string works. Persist the key and
final facts together when queuing a report for retry.

`recording_id` refers to an existing shared recording, independent of its
encoding, release or physical file. Reporting does not require an available
local file, so clients can submit delayed/offline listening. An unknown
recording receives 422.

`started_at` is the playback start in **Unix seconds**. It must be nonnegative
and no more than five minutes ahead of the server's clock. Older timestamps
are accepted for delayed reports. `received_at` is assigned by the server as
UTC ISO 8601 with millisecond precision; it is separate from playback time.

`listened_ms` is positive cumulative media time actually played. Exclude
pauses, buffering, downloads, and positions skipped by seeking. When duration
is known, cap cumulative listened time at that duration; a new replay gets a
new event ID. `duration_ms` is optional/null when unavailable; a supplied
value must be positive and at least `listened_ms`.

These are client-reported facts, not proof of listening. No HTTP download,
Range request, preload or successful file transfer automatically creates an
event. History does not change favorites, reactions, or shared catalog data.

## Full and short events

The server assigns `kind` when accepting the report:

- Known duration: `full` at half its duration, rounded up to the next
  millisecond, or four minutes, whichever threshold is shorter.
- Unknown duration: `full` at four minutes.
- Anything below that threshold: `short`.

Both kinds remain in history. `full` is a qualifying listen, not a claim that
100% of the recording played. For example, 90 seconds of a three-minute track
qualifies, while five seconds does not. For odd millisecond durations the
threshold rounds up, avoiding qualification below half. Classification is
stored with the original facts; retries/read operations do not recalculate it.

## Retries and reads

A new event returns 201 and Location, for example `/api/v1/me/history/1`.
An identical retry with the same account/event key returns 200 with the same
item ID, listening facts and `received_at`. Retrying does not duplicate the
event or move it in the timeline. Omitting duration and sending null both mean
unknown duration. Reusing a key with a different recording, start, listened
time or duration returns 409 without modifying the accepted event. Concurrent
retries are serialized atomically. Different users have independent key
namespaces.

Events are final, immutable reports. Progress updates, heartbeats and pause
checkpoints must not be submitted as changed versions of the same event.
Submit the final accumulated facts when the player finishes, stops or switches
tracks; a temporary pause followed by resume remains the same playback.

Lists return `items`, `limit`, `offset`; default limit is 50, allowed range
1–100, offset nonnegative. History sorts by `started_at` descending, then ID
descending for equal start times. A delayed report appears at its playback
time, not at upload time. Each item contains `id`, `event_id`, `recording_id`,
current catalog `title`, `started_at`, `listened_ms`, `duration_ms`, `kind`,
and `received_at`. The title reflects later catalog corrections, including on
retry; the accepted listening facts remain intact.

```sh
curl -X POST -H "Authorization: Bearer $PERSONAL_API_KEY" \
  -H 'Content-Type: application/json' --data-binary @dev/playback.json \
  http://127.0.0.1:3000/api/v1/me/history
curl -H "Authorization: Bearer $PERSONAL_API_KEY" \
  'http://127.0.0.1:3000/api/v1/me/history?limit=50&offset=0'
```

Only the authenticated account determines ownership. Unknown fields, including
`user_id` or `kind`, are rejected. Another user's event ID receives the same
404 as a nonexistent event. There is no account selector in the query.
Revocation/password changes do not erase accepted events; expired/revoked
credentials cannot submit or read history until the client authenticates again.

Successful responses use `Cache-Control: private, no-store`. Invalid facts
return JSON 422, malformed JSON/IDs/query parameters 400, missing JSON content
type 415, and bodies exceeding 4096 bytes 413. Authentication precedes input
validation; invalid credentials receive 401 with the Bearer challenge.

Playback statistics, aggregate ratings, history editing/deletion, and
now-playing/progress state remain separate features.

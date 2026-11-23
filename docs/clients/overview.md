# Your first API requests

Connect to a server, sign in and read the first bytes of a track. You'll
need Bash, `curl`, an account and at least one audio file in the library.
[Set up a test server](../guide/quick-start.md)

## 1. Check the server address

Use the address the listener would type into the player. Append `/api/v1`
once when you configure the HTTP client.

```sh
SERVER=http://localhost:3000
API="$SERVER/api/v1"
curl -fsS "$API/capabilities"
```

This request works before sign-in. The response includes `api_version`,
`server_version`, catalog visibility and limits for pages, batch calls
and collections. Use `downloads_enabled` and `automation_enabled` to
decide which controls to show.

## 2. Sign in and check the account

Paste one line of JSON with your username and password, for example
`{"name":"alice","password":"your-password"}`. Bash reads it without
echoing it to the terminal.

```sh
read -rsp 'Login JSON: ' login_json
printf '\n'
printf '%s' "$login_json" | curl -fsS "$API/auth/login" \
  -H 'Content-Type: application/json' --data-binary @-
unset login_json
```

Copy the `access_token` value. For the remaining examples, paste it at
the `Token:` prompt.

```sh
read -rsp 'Token: ' ACCESS_TOKEN
printf '\n'
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" "$API/me"
```

You can now see the signed-in account and its role. If you get `401`,
check the server address and token. Save `expires_at` so the client can
prompt for sign-in when the session ends.
[How sessions and errors work](./authentication.md)

## 3. Find a recording and choose a file

```sh
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" \
  "$API/recordings?limit=20&offset=0"
```

Pick a recording from `items`. If `has_more` is `true`, advance `offset`
by the number of items you read to get the next page. For an empty
catalog, [scan a music folder](../guide/library.md) first.

Replace `12` with the recording ID.

```sh
RECORDING_ID=12
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" \
  "$API/recordings/$RECORDING_ID/files"
```

Pick a file from `items` in a format your player supports. Audio requests
use both the recording ID and the selected file ID.

## 4. Try the stream

Replace `7` with the file ID. This request reads up to 1024 bytes and
prints the response headers.

```sh
FILE_ID=7
curl -fsS -D - -o /dev/null \
  -H "Authorization: Bearer $ACCESS_TOKEN" -H 'Range: bytes=0-1023' \
  "$API/recordings/$RECORDING_ID/files/$FILE_ID/stream"
```

Expect `206 Partial Content` and a `Content-Range` header. Your audio
engine can use the same stream. An HTML audio element can use a
[temporary link](./playback.md#browser-playback).

## What to build next

- [Authentication](./authentication.md) for sign-in, sign-out and errors.
- [Catalog and playback](./playback.md) for search, seeking and artwork.
- [Personal collections](./collections.md) for favorites, queue and history.
- [Discovery and downloads](./discovery.md) for finding new music.

See the [API reference](../reference/api.md) for the request and response schemas.

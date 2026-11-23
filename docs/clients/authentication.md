# Authentication and errors

Keep a separate connection for each server and account. After sign-in,
use the token with `GET /api/v1/me` to check the account and its role.
[Try sign-in with curl](./overview.md)

## Sign in, keep a token and sign out

Send the username and password to `POST /api/v1/auth/login`. The response
contains `access_token`, `token_type: "Bearer"` and `expires_at` in Unix
seconds. Send the token on later requests in this header:

```http
Authorization: Bearer TOKEN
```

| Credential | When it expires | How it is revoked |
| --- | --- | --- |
| Password session | At `expires_at`, after 30 days by default. | `POST /api/v1/auth/logout` ends the current session. A password reset ends all the account's password sessions. |
| Personal API key | When explicitly revoked. | An administrator revokes the account's keys. A password reset leaves them valid. |

A session token is opaque. Read `expires_at` from the response and
prompt the user to sign in again when the session expires. Shprotify
issues a new session on password sign-in.

> [!IMPORTANT]
> Store the token in the operating system's credential store when available.
> Keep it out of logs and send it only to the server that issued it.

## What works before sign-in

Anyone can call `GET /api/v1/capabilities`. Use `public_catalog` to
decide whether to offer browsing before sign-in. Personal actions and
issuing audio links require a token. Server administration requires the
`admin` role. Another account's private resource usually returns `404`.
[Access settings for administrators](../guide/accounts.md)

## Handle errors

For errors from Shprotify, branch on the HTTP status and `error.code`.
You can show `error.message` to explain what happened. A `HEAD` response
has no body. A failed connection or proxy response may have no JSON to parse.

| Status | What the client can do |
| --- | --- |
| `400`, `413`, `415` | Check the request parameters, body size or content type. |
| `401` | Prompt for sign-in if the token is no longer valid. |
| `403` | Explain what permission the action requires. |
| `404` | Show that the resource is unavailable. |
| `409` | Reload stale versioned data, or check why a background operation conflicts. |
| `422` | Show which values need to change. |
| `429` | Wait before retrying. Use `Retry-After` when present. |
| `5xx` | Show a server failure and let the listener try again later. |

A successful status request may still report a failed job. Check the job's
own outcome. If the response to a write was lost, check whether the server
saved it before retrying.

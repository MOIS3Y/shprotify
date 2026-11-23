# Users and access

Everyone on the server shares one music library. Each user has their own
favorites, playlists, queue and listening history. Once you've created an
administrator in the [quick start](./quick-start.md), you can invite others.

## Add a user

Open a terminal in the folder containing `compose.yaml`. Replace `bob`
with the new username and run both commands:

```sh
docker compose exec shprotify shprotify user create --name bob
docker compose exec shprotify shprotify user password --name bob
```

The first command creates a member account. The second asks you to enter
a password twice. The terminal hides the characters as you type.

To sign in through a client, the user needs the server address, their
username and password. Accounts are created by an administrator.
There's no public sign-up page.

> [!NOTE]
> The command that creates an account shows its personal API key once.
> Keep it private like a password. Users sign in with their password.

## Choose a role

| Role | What it can do |
| --- | --- |
| Member (`member`) | Listen to music, make playlists and use recommendations. |
| Administrator (`admin`) | Do everything a member can, plus manage accounts, scan folders and update catalog data. |

To make this user an administrator:

```sh
docker compose exec shprotify shprotify user role --name bob --role admin
```

To make them a regular member again:

```sh
docker compose exec shprotify shprotify user role --name bob --role member
```

The new role takes effect immediately. The user can keep using the same session.

## Set how long users stay signed in

A password session lasts 30 days by default. Users sign in again once it
expires. To make new sessions last seven days, use one of these settings:

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_AUTH__SESSION_TTL_SECONDS: "604800"
```
```toml [TOML]
[auth]
session_ttl_seconds = 604800
```
:::

The value is in seconds. Existing sessions keep their original expiry.
Using the client doesn't extend a session.
[How to apply settings](./configuration.md)

## Recover access

If a user forgets their password, set a new one with `user password`.
You can also use this to reset your administrator password from a terminal
on the server. Replace `bob` with the right username:

```sh
docker compose exec shprotify shprotify user password --name bob
```

This signs the user out of all password sessions. Their personal API keys
keep working.

If a personal API key is lost or exposed, revoke the account's keys:

```sh
docker compose exec shprotify shprotify user revoke --name bob
```

This revokes every personal API key for the account and keeps its password
sessions and data. To issue a replacement, use `user token --name bob`.
[How API authentication works](../clients/authentication.md)

## Who can see the library?

The Compose file from the quick start keeps the catalog private. Users
have to sign in to browse the music, search and see cover art.

To let guests browse the catalog, set
`SHPROTIFY_LIBRARY__PUBLIC_CATALOG: "true"` in Compose or
`public_catalog = true` under `[library]` in TOML. The built-in app default
allows public browsing. Set it to `false` explicitly for a private server.

- Playback requires valid credentials or a temporary audio link from a player.
- Signed-in users can view published playlists. The owner can edit them.
- Favorites, history and personal jobs belong to the account. The server
  operator has access to the database where those are stored.

If your web player uses a different domain, add its address to the
[CORS settings](./proxy.md#if-a-web-player-uses-another-domain).

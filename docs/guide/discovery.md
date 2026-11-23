# Find music

Start with what you already listen to. Shprotify can suggest tracks from
your library, bring back old favorites or help you find music you don't
have yet. You can create a separate feed for each.

## Choose a feed

| What you're looking for | Which feed to use | Source |
| --- | --- | --- |
| Something that suits your taste | A personal feed suggests tracks from your library. With external sources enabled, it can also suggest related music and public playlists. | Your library, optionally Last.fm and ListenBrainz |
| Something you haven't played in a while | Rediscover brings back favorites you haven't played fully in the past 30 days. | Your library |
| Charts or a genre to explore | Try a global or country chart, or a tag feed such as `jazz`. You can also search by title or find music similar to a particular track or artist. | Last.fm |
| A ready-made playlist | Search public playlists by title or description. If you already know the playlist ID, open it directly. | ListenBrainz |
| New releases from artists you like | Use the new releases feed. Shprotify looks up artists from your profile who have a confirmed MusicBrainz ID. | MusicBrainz |

## How recommendations are chosen {#how-recommendations-work}

**What you like.** A personal feed uses likes from your library and
recommendations, qualifying listens and the artists you follow. Recent
listens count more than older ones. A quick skip on its own doesn't
count as a dislike. If your history is short, start with favorite artists
and tags for external searches. Everyone gets their own recommendations
and feedback, even when they share a library.

**Where Shprotify looks.** Your library is enough to get started. For
example, if you often listen to an artist, the feed can suggest another
of their recordings from your collection. With Last.fm enabled, Shprotify
looks for related tracks and artists and can pull in tags and charts.
It can search ListenBrainz for public playlists by tag or artist. A
separate new releases feed queries MusicBrainz using confirmed artist IDs.
Your listening history stays on your server. External services receive
search terms such as track titles, artist names or tags.

**Why a suggestion might be unavailable to play.** Shprotify checks
whether a suggestion matches your library. If it comes with a
MusicBrainz ID, the server looks for an exact ID match. With no ID,
it can use a unique match by title and artist credits. If there's no
local recording or audio file yet, mark it as wanted and come back later.

## Enable external sources

The server administrator can enable any of these sources:

- **Last.fm.** Create an application key and keep it in the server settings.
  [Get a Last.fm key](https://www.last.fm/api/account/create)
- **ListenBrainz.** Enable the source to search and read public playlists.
  [ListenBrainz documentation](https://listenbrainz.readthedocs.io/en/latest/users/api/playlist.html)
- **MusicBrainz.** Enable the source for new releases and confirm the
  MusicBrainz IDs of artists in your catalog. [How to link MusicBrainz data](./metadata.md)

The example enables all three. Replace `you@example.org` with your contact
address. To leave Last.fm disabled, omit the key or leave it blank.
Use `false` to disable ListenBrainz or MusicBrainz.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_DISCOVERY__ENABLED: "true"
SHPROTIFY_DISCOVERY__USER_AGENT: "shprotify/0.1.0 (mailto:you@example.org)"
SHPROTIFY_DISCOVERY__LASTFM_API_KEY: "YOUR_APPLICATION_KEY"
SHPROTIFY_DISCOVERY__LISTENBRAINZ_ENABLED: "true"
SHPROTIFY_DISCOVERY__MUSICBRAINZ_ENABLED: "true"
```
```toml [TOML]
[discovery]
enabled = true
user_agent = "shprotify/0.1.0 (mailto:you@example.org)"
lastfm_api_key = "YOUR_APPLICATION_KEY"
listenbrainz_enabled = true
musicbrainz_enabled = true
```
:::

If you already have a `[discovery]` section, add the settings there.
[How to apply settings](./configuration.md)

## Create and refresh a feed

If your client supports recommendations, choose a feed type, enter a
tag or artist if needed and wait for the results. Creating a feed starts
the first search. You can refresh the same feed later.

::: details Create a personal feed manually in Swagger
Sign in with your account and use **Authorize** as shown in the
[quick start](./quick-start.md#sign-in). Under **Discovery**, run
`POST /api/v1/me/discovery/feeds` with this request body:

```json
{"kind":"personal"}
```

The response contains the job `id` and the `feed_id`. Check the job using
`GET /api/v1/me/discovery/jobs/{id}`. Once its status is `completed`,
use the `feed_id` in `GET /api/v1/me/discovery/feeds/{id}/items`.
To refresh the same feed later, run `POST /api/v1/me/discovery/feeds/{id}/refresh`.
:::

## What to do with a suggestion

- **Play it.** If the matched recording has an audio file in your library,
  you can open it in a player.
- **Save it for later.** Mark it as wanted. It stays in your wishlist when
  the feed refreshes. If downloads are enabled, you can search separately
  for audio for the track. [YouTube downloads](./downloads.md)
- **Hide it.** Remove a suggestion from your own results.
- **Make a playlist.** Export the feed to make a private playlist from
  eligible recordings in your library. Later refreshes leave it unchanged.

> [!NOTE]
> External feeds can suggest tracks, albums or entire playlists. A source
> link opens the music's page. To play it through Shprotify, you'll need
> an audio file in your library.

## If a feed doesn't refresh

By default, wait at least 60 seconds between refreshes on the same account.
Shprotify can reuse responses from external services for up to an hour,
so a refresh may show the same results.

If a source doesn't respond, wait and try again. A failed refresh keeps
the previous feed results. If a new releases feed is empty, check that
the artists in your profile have confirmed MusicBrainz IDs.

For regular updates, set up a [feed schedule](./automation.md).
If you're building a client, see [Discovery and downloads](../clients/discovery.md).

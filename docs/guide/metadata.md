# Fix tags and add covers

If an album title looks wrong or a track is missing its cover, an
administrator can fix it in the Shprotify catalog. Start with the source
that suits what you want to change.

## Where the data comes from

| Source | What you can get |
| --- | --- |
| Tags in your audio files | A scan reads the title, artist, album, track number and disc number. If a file already contains an image, you can extract it separately. |
| [MusicBrainz](https://musicbrainz.org/doc/MusicBrainz_API) | Find an artist, a particular album edition or a recording by title or exact MusicBrainz ID (MBID). |
| [Cover Art Archive](https://musicbrainz.org/doc/Cover_Art_Archive/API) | Get the front cover for a particular album edition once you've linked it to MusicBrainz. |
| [AcoustID](https://acoustid.org/webservice) | Identify a recording by its sound when the title is unknown or the tags are wrong. |
| Your own edits | Correct a name or artist credit in the catalog, or upload your own image. |

Changes are stored in the catalog. The tags and artwork in your audio
files stay as they are. You can edit details, upload an image or extract
one from a file with online lookup turned off.

## Enable MusicBrainz lookup

Replace `you@example.org` with your own contact address. Shprotify sends
it in the `User-Agent` header so MusicBrainz can reach you if there's a
problem with your requests. Searching requires no API key.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_METADATA__ENABLED: "true"
SHPROTIFY_METADATA__USER_AGENT: "shprotify/0.1.0 (mailto:you@example.org)"
```
```toml [TOML]
[metadata]
enabled = true
user_agent = "shprotify/0.1.0 (mailto:you@example.org)"
```
:::

If you already have a `[metadata]` section, add the settings there.
[How to apply settings](./configuration.md)

> [!NOTE]
> These settings enable metadata and cover art lookup. Set up Last.fm
> and other recommendation sources separately. [Find music](./discovery.md)

## Find and review a match

If your client can manage metadata, choose an artist, album or track in
the app and start a lookup. Check the title and artist credits. For an
album, check the edition too. Confirm the match and choose which details
to copy. You can link an album to MusicBrainz while keeping your own
title and credits.

::: details Search for an album manually in Swagger
[Sign in as an administrator](./quick-start.md#sign-in). Under **Releases**,
find the album with `GET /api/v1/releases` and take its `id`. The lookup
operations are under **Admin: Enrichment**.

1. Run `POST /api/v1/metadata/{kind}/{id}/jobs` with `kind=release` and the
   album ID. Use `{"action":"search"}` as the body to find matches for the
   current title.
2. Copy the job ID and open `GET /api/v1/metadata/jobs/{id}`. When the job
   completes, you'll see the suggestions in `candidates`.
3. Pick the match. Under **Admin: Metadata**, use
   `GET /api/v1/metadata/release/{id}` to read the album's current `version`.
   Then run `POST /api/v1/metadata/candidates/{id}` with the candidate ID.
   Replace `1` below with the version you just read.

   ```json
   {"version":1,"name":false,"credits":false}
   ```

This confirms the MBID while keeping your title and artist credits.
If the album has changed, fetch its new version and review the match
again before applying it.
:::

Lookups, including [scheduled ones](./automation.md), produce suggestions.
The catalog changes once you confirm a match.

## Add cover art

- **From an audio file.** Choose a track or album and one of its audio files.
  Request the embedded image (`embedded_artwork`). If a picture is marked
  as the front cover in the tags, Shprotify will suggest that one.
- **From Cover Art Archive.** First confirm the MBID for the album edition
  you want. Then request its cover (`cover_art`). Shprotify suggests the
  front cover if the archive has one for that edition.
- **From your computer.** Upload your own image for an artist, album or
  track in a client that manages artwork, or use
  `PUT /api/v1/metadata/{kind}/{id}/artwork` under **Admin: Metadata**.

You can preview a suggested picture before applying it. Shprotify accepts
JPEG and PNG images up to 5 MiB and 4096 pixels per side. Saved images
are stored in `shprotify.db`.

## Identify a track by sound (AcoustID)

If searching by name doesn't help, try an audio fingerprint. The Docker
image includes `fpcalc`, which computes the fingerprint on your server.
Shprotify sends the fingerprint and track duration to AcoustID. The
audio file stays on your server. Shprotify looks up the matches in MusicBrainz.

Enable online lookup as shown above and add the key you get when you
register an application in AcoustID. Use an application key.
Personal keys are for submitting fingerprints. [AcoustID documentation](https://acoustid.org/webservice)

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_METADATA__ACOUSTID_KEY: "YOUR_APPLICATION_KEY"
SHPROTIFY_METADATA__FPCALC_PATH: "fpcalc"
```
```toml [TOML]
[metadata]
acoustid_key = "YOUR_APPLICATION_KEY"
fpcalc_path = "fpcalc"
```
:::

Choose the recording and its audio file, then run a fingerprint lookup
(`fingerprint`). Review the match before applying it, just as you would
with a text search. An empty `fpcalc_path` disables fingerprint lookup.

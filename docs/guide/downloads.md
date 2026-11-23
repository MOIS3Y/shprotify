# Download from YouTube

If a track is missing from your library, Shprotify can search for it
on YouTube. You choose a video and the server saves its audio to the
shared library. An administrator enables the feature. Once it's on,
regular members can use it too.

> [!NOTE]
> YouTube downloads are experimental. Changes to YouTube search or audio
> delivery can break downloads. You can still listen to music from your
> library. [Music and rights](./introduction.md)

## Enable downloads

Enable downloads and the YouTube source. This example uses
`/data/downloads` inside the container. In the quick-start setup that
maps to `./data/downloads/` on the host.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_DOWNLOADS__ENABLED: "true"
SHPROTIFY_DOWNLOADS__YOUTUBE__ENABLED: "true"
SHPROTIFY_DOWNLOADS__DIRECTORY: "/data/downloads"
```
```toml [TOML]
[downloads]
enabled = true
directory = "/data/downloads"

[downloads.youtube]
enabled = true
```
:::

If these sections or keys already exist, edit them in place. The Docker
image includes `yt-dlp`, FFmpeg and Deno.
[How to apply settings](./configuration.md)

> [!TIP]
> Keep downloads separate from folders Shprotify scans. The server adds
> downloaded tracks to the catalog itself. If the download folder is
> missing, Shprotify creates it when it can write to the parent folder.

## Find and download a track

If your client supports downloads, start from a track in
[your recommendations](./discovery.md) or an existing catalog entry.
You can also search by title and artist.

1. Search YouTube. Compare the video title, channel and duration to find
   the original, a studio recording or the version you want.
2. Pick one result and start the download. Shprotify downloads the audio,
   checks the file and adds it to the library.
3. When the job finishes, open the track in the regular catalog.
   It's ready to play without another scan.

Search history and download progress are private to the account that
started them. The finished track is available to everyone on the server.
For a recommended album or playlist, pick a track and search for
its title and artist.

::: details Download a track manually in Swagger
[Sign in with your account](./quick-start.md#sign-in) and open
**Downloads**. This example searches by title and artist.

1. Run `POST /api/v1/me/downloads/searches` with the body below.
   Replace the title and artist with your own.

   ```json
   {"kind":"query","title":"Track title","artists":["Artist name"]}
   ```

2. Copy the job `id` from the response and use it with
   `GET /api/v1/me/downloads/{id}`. When the status is `completed`,
   request the results with `GET /api/v1/me/downloads/{id}/sources`.
3. Pick a result and take its `id`. Run `POST /api/v1/me/downloads`,
   replacing `17` with the ID of the chosen result.

   ```json
   {"source_id":17}
   ```

4. Check the new job with `GET /api/v1/me/downloads/{id}`. When it
   completes, `recording_id` identifies the track in the shared library.
:::

## Where the files go

Finished files live in `./data/downloads/objects/`. The `.staging/` folder
under downloads holds temporary job files. Cancel a download from the
client before it reaches the import stage.

The file format depends on the audio found on YouTube. You might get
Opus or AAC, for example. Your player needs to support the resulting format.

## If a download fails

| What happened | What to try |
| --- | --- |
| Downloads are unavailable in the client | Check that both switches are on and the settings have been applied. Check that the client supports downloads. |
| Search came back empty or the video is unavailable | Refine the title and artist, start a fresh search and pick another result. |
| The server won't start another job | Wait for an active download to finish and try again. |
| A download stopped or the file failed validation | Read the job message. Check free disk space and configured limits. If search and downloads fail for several tracks, check whether there's a newer image. |

By default, up to two search or download jobs run at once across the
server. Each user can have three queued or active jobs. The default
limits are 256 MiB per file, 30 minutes of audio and 10 minutes to
download it. [All download limits](./configuration.md#complete-reference)

To stop accepting downloads, set `downloads.youtube.enabled = false`
in TOML or `SHPROTIFY_DOWNLOADS__YOUTUBE__ENABLED: "false"` in Compose.
Keep the download folder configured so previously imported tracks remain playable.
Disk use depends on the size of the whole collection.

If you're building a client, see [Discovery and downloads](../clients/discovery.md).

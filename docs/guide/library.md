# Add music

Put files in your music folder and ask Shprotify to update the library.
The scan reads tags so you can browse by artist and album.
Your original files stay where they are.

## Add files to the library {#import-a-folder}

If your client can scan the library, start the scan from the app.
The steps below show how to do it manually in Swagger.

1. Copy music into `./music/` next to `compose.yaml`. If you mounted
   an existing music folder during setup, put the files there.
2. Open Swagger at `http://localhost:3000/docs` and
   [sign in as an administrator](./quick-start.md#sign-in).
3. Under **Admin: Library**, run `POST /api/v1/library/scans`.
   Copy the `id` from the response.
4. In the same section, open `GET /api/v1/library/scans/{id}`. Enter
   that ID and check the status until it reads `completed`.
5. Under **Recordings**, run `GET /api/v1/recordings`. The results
   are in the `items` list.

> [!TIP]
> Use `shprotify scan` to inspect file tags and audio properties in the
> terminal. This command leaves the catalog unchanged.

### If a track is missing

Check the response from `GET /api/v1/library/scans/{id}`. The `roots`
field shows which folders the server scanned, and `imported` counts
new files. If the status is `failed`, check `message` to see why it stopped.
For errors about individual files, open `GET /api/v1/library/scans/{id}/errors`
with the same ID. The report lists the files that had problems.
Other tracks may still have been added.

- If a folder is missing from `roots`, check how it is mounted in Compose
  and which path is listed in Shprotify's settings.
- If the file is in the folder, check that the container can read it.
  The scanner skips symbolic links inside library folders.
- If a scan is already running, wait for it to finish. The server runs
  one scan at a time.

## Connect another folder {#connect-another-directory}

For example, if some of your music is in `/srv/archive`, add this line
to the existing `volumes` list for the `shprotify` service:

```yaml
- /srv/archive:/archive:ro
```

Docker makes the folder available to the server at `/archive` in read-only
mode. List both library folders wherever you keep your settings:

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_LIBRARY__DIRECTORIES: '["/music", "/archive"]'
```
```toml [TOML]
[library]
directories = ["/music", "/archive"]
```
:::

If you already have a `[library]` section, edit its `directories` line.
Use separate folders. The scanner already reads subfolders of every
folder you add.

Apply the changes with `docker compose up -d` and run another scan.
For more on paths inside the container, see [Settings and files](./configuration.md).

## Update the library

Run the same scan after adding files or editing tags. It skips files
that haven't changed. To reread every file, set the `force` parameter
to `true` when starting the scan. For files it already knows, the server
updates file information and keeps the metadata already in the catalog.

If a file is removed from disk, its track stays in the catalog.
You can restore the file to its old path to make it playable again.

If you add music regularly, you can [schedule scans](./automation.md).

## Where downloads and covers are stored

Put your own files in the library folders. If you enable
[YouTube downloads](./downloads.md), Shprotify saves the resulting tracks
under `./data/downloads/` and adds them to the catalog after downloading.
Keep that folder in the download settings so existing tracks stay
available if you disable new downloads.

Saved artwork is stored in `shprotify.db`. Extracting a cover from an audio
file is a separate action from scanning the library. See [Tags and covers](./metadata.md).

# Backups and updates

A copy of `data/` preserves your catalog, accounts, playlists, history,
cover art and downloads managed by Shprotify. Save your setup files and
original music too so you can restore the whole library.

## Make a backup

These commands assume the [quick-start setup](./quick-start.md).
Run them from the folder containing `compose.yaml`.

| What to save | Why |
| --- | --- |
| All of `data/` | It holds the SQLite database, cover art and downloads. The copy also includes any `-wal` and `-shm` files beside the database. |
| `compose.yaml`, plus `.env` and `config/` if you use them | These preserve the image choice, folder paths and server settings. |
| `music/` or the folder containing your original collection | The database holds catalog entries. The audio stays in your folders and can be backed up separately. |

If you changed the database or download paths, make sure the backup
includes those folders. [Where the files are](./configuration.md#where-the-files-are)

Record the running version so you know which image to use when restoring:

```sh
docker compose exec shprotify shprotify --version
```

Stop Shprotify while copying the files. Add `.env` and `config/` to the
`tar` command if you use them. This creates the archive next to the
installation folder.

```sh
backup="../shprotify-$(date +%Y%m%d-%H%M%S).tar.gz"
docker compose stop shprotify && (
  umask 077
  sudo tar -czf - compose.yaml data > "$backup"
)
```

Start the server again after copying. If `tar` reported an error, make
a new backup. You can check the finished archive as shown below.

```sh
docker compose start shprotify
tar -tzf "$backup"
```

Store the archive somewhere private. It contains personal data and may
contain keys for external services.

> [!NOTE]
> Stop the server before making a `tar` backup and include all of `data/`.
> A running SQLite database can have committed changes in its `-wal` file.
> [How SQLite stores the WAL](https://sqlite.org/wal.html#the_wal_file)

## Restore from a backup

1. Stop the old container with `docker compose stop shprotify` if it is
   still running. Keep its data folder as a separate copy.
2. From the parent folder, extract the archive into a **new empty folder**.
   Replace the filename with the name of your backup.

   ```sh
   mkdir shprotify-restore && cd shprotify-restore &&
     sudo tar --numeric-owner -xzpf ../shprotify-YYYYMMDD-HHMMSS.tar.gz
   ```

3. Restore your original audio files. Keep the same paths **inside** the
   container, such as `/music/`. Check that UID 1000 can read and write
   `data/` and read the audio files.
4. Set `.env` to [the image version used for the backup](./deployment.md).
   Run `docker compose up -d` from the restored folder.

Check that you can sign in, browse the catalog and play a few tracks.
The restored database reflects the time of the backup. You can scan for
new tracks again. Playlists, accounts and history come from the database copy.

## Update Shprotify

1. Read the release notes and make a backup.
2. If you use a fixed tag, change it in `.env` to the new release tag.
   If you follow `latest`, keep it.
3. Pull the image and start the container with it.

   ```sh
   docker compose pull shprotify && docker compose up -d shprotify
   ```

When the server is ready, check that it responds with `{"status":"ok"}`.
If you use Traefik, use your HTTPS address instead.

```sh
curl -fsS http://localhost:3000/api/v1/health
```

Then check that you can sign in and play a track.

> [!IMPORTANT]
> A new version can change the database schema on startup. If you need to
> roll back, run the previous image with the data backup from before the update.

If the server doesn't start or respond, see [Troubleshooting](./troubleshooting.md).

# Settings and files

After following the [quick start](./quick-start.md), you can change
Shprotify's settings in `compose.yaml`. If you prefer a separate settings
file, [use a TOML file](#use-a-toml-file).

## Where the files are

This is the installation folder on the host. You create `.env` and
`config/` if you need them. The contents of `data/` appear as the
server uses them.

```text
shprotify/
├─ compose.yaml
├─ .env                    # optional: image tag
├─ config/
│  └─ shprotify.toml       # optional: server settings
├─ music/                  # your audio files
└─ data/
   ├─ shprotify.db         # catalog, artwork and user data
   ├─ shprotify.db-wal     # SQLite working files
   ├─ shprotify.db-shm
   ├─ downloads/
   │  ├─ objects/          # downloads added to the library
   │  └─ .staging/         # downloads in progress
   └─ cache/               # cache for external tools
```

Compose maps each path on the host to a path inside the container:

| On the host | In the container | Access |
| --- | --- | --- |
| `./data/` | `/data/` | Read and write for UID/GID 1000 |
| `./music/` | `/music/` | Read-only |
| `./config/shprotify.toml` | `/config/shprotify.toml` | Read-only when using TOML |

> [!TIP]
> If you mount `/srv/music:/music:ro`, use `/music` in Shprotify's
> settings. That's where the server sees the files inside the container.

The container's working directory is `/data` by default. Relative
paths in the settings start there. Use absolute paths inside the
container to make it clear where each setting points.

## Change a setting in Compose

For example, to make new login sessions last seven days, add this line
under the existing `environment` block for the `shprotify` service:

```yaml
SHPROTIFY_AUTH__SESSION_TTL_SECONDS: "604800"
```

`SHPROTIFY_` is the prefix, and `__` separates the `auth` section
from the `session_ttl_seconds` setting. Quote values in YAML.
Pass a list as a single string:

```yaml
SHPROTIFY_LIBRARY__DIRECTORIES: '["/music", "/archive"]'
```

For each folder, tell Docker where to mount it from.
See the [example with two folders](./library.md#connect-another-directory).

Save `compose.yaml` and apply the changes:

```sh
docker compose up -d
```

You can use the `.env` file next to `compose.yaml` to select an image
with `SHPROTIFY_IMAGE`. The examples pass app settings to the container
through `environment`. [How to choose an image tag](./deployment.md)

## Use a TOML file

Create `config/shprotify.toml` in the installation folder. The container
user (UID 1000) needs read access to this file.

<<< ../public/examples/shprotify.toml{toml}

Replace the contents of `compose.yaml` with the example below.
Keep your existing `data/` and `music/` folders. This version mounts
the TOML file and starts the server with `--config /config/shprotify.toml`.

<<< ../public/examples/compose.config.yaml{yaml}

Apply the new `compose.yaml` with `docker compose up -d`. After that,
if you only change the TOML file, restart the server:

```sh
docker compose restart shprotify
```

## Which value takes effect?

1. The environment variable, if set.
2. The value in the file passed with `--config`.
3. The built-in default.

Shprotify reads TOML from the path given with `--config`. If that
file is missing or unreadable, startup fails with an error.

> [!NOTE]
> The image sets environment variables for the server's bind address,
> database path, downloads folder and log format. To change one,
> set its value under `environment` in `compose.yaml`.
>
> - `SHPROTIFY_HTTP__BIND`
> - `SHPROTIFY_DATABASE__PATH`
> - `SHPROTIFY_DOWNLOADS__DIRECTORY`
> - `SHPROTIFY_LOGGING__FORMAT`

## Complete reference

The example below lists every setting with comments. Copy the settings
you want to change into your own file.

<<< ../../examples/shprotify.toml{toml}

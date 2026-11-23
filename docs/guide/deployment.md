# Image tags and Docker commands

The container name is `ghcr.io/mois3y/shprotify`.

| Tag | Use |
| --- | --- |
| `latest` | Follow the newest published release |
| `0.1.0` | Stay on a specific release until you change the tag |

Use the exact tag shown on the release page. Tags only become pullable
once that release's image has been published. A `manifest unknown` or
`denied` response is an image/registry problem, not a configuration error.

## Pin a version in Compose

The [installation example](./quick-start.md) defaults to `latest`.
Create `.env` alongside `compose.yaml` to select a release:

```dotenv
SHPROTIFY_IMAGE=ghcr.io/mois3y/shprotify:0.1.0
```

This variable is read by Compose to select the image. It is not an
application setting and does not need to be passed into the container.

## Run without Compose

Create `data` and `music` and give `data` to UID/GID 1000 as in the
installation guide. Then, from that directory:

```sh
docker run -d --name shprotify --restart unless-stopped \
  --stop-timeout 90 \
  -p 127.0.0.1:3000:3000 \
  -v "$PWD/data:/data" \
  -v "$PWD/music:/music:ro" \
  -e SHPROTIFY_HTTP__BIND=0.0.0.0:3000 \
  -e SHPROTIFY_DATABASE__PATH=/data/shprotify.db \
  -e 'SHPROTIFY_LIBRARY__DIRECTORIES=["/music"]' \
  -e SHPROTIFY_LIBRARY__PUBLIC_CATALOG=false \
  -e SHPROTIFY_DOCS__ENABLED=true \
  ghcr.io/mois3y/shprotify:latest
```

Account commands use `docker exec -it shprotify shprotify user ...`.
See [Accounts](./accounts.md) for complete commands.

## Host and container addresses

`127.0.0.1:3000:3000` publishes the port only on the Docker host's loopback
interface. `0.0.0.0:3000` in `SHPROTIFY_HTTP__BIND` is the listener
**inside** the container. These are different settings.

To allow other machines on a trusted LAN, use `3000:3000` for the port
mapping. For access over the internet, use [HTTPS](./proxy.md).

## Updates

Follow [Backups and updates](./operations.md). Pulling an image does not
replace a running container until you recreate it. Avoid automatic updates
if you want to choose when database migrations run.

To build an image yourself, see [Nix](./nix.md).

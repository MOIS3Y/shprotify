# Troubleshooting

Start by checking that the container is running and the server responds.
Run these commands from the folder containing `compose.yaml`:

```sh
docker compose ps -a shprotify
docker compose logs --tail 100 shprotify
curl -fsS http://localhost:3000/api/v1/health
```

If you use Traefik, check your HTTPS address instead. A response of
`{"status":"ok"}` tells you the server is responding. If the problem
persists, find the matching symptom below.

## The server won't start or respond

| What you see | What to check |
| --- | --- |
| The image won't pull | Check its name and tag, registry access and whether the tag was published. [Image tags](./deployment.md) |
| `Permission denied` for the database or downloads | Check ownership and permissions on the folder mounted at `/data`. The container runs as UID/GID 1000. [Settings and files](./configuration.md) |
| Docker says the port is already in use | Check the host port under `ports`. If you change it, use the new port in your client's server address. Keep the listener inside the container at `0.0.0.0:3000` as in the example. |
| HTTPS requests fail | Check the route from your domain to the container and the certificate. [Traefik troubleshooting](./proxy.md#troubleshooting) |

## The server responds, but music is unavailable

| What you see | What to check |
| --- | --- |
| The catalog is empty | Check that the container can read the music folder. Run a scan after copying files and open the [scan report](./library.md#if-a-track-is-missing). |
| A track is listed but won't play | Check that the file is still at its original path and that the player supports its format. [Add music](./library.md) |
| The client gets `401 Unauthorized` | Check the client's server address and sign in again. For manual requests, check the `Authorization` header. [Users and access](./accounts.md) |
| The browser reports a CORS error | Use the exact address of the player page in the [CORS settings](./proxy.md#if-a-web-player-uses-another-domain). |
| Metadata lookup, recommendations or downloads fail | Check the source settings and the job message. Music already in your library remains available while an external service is down. [Metadata](./metadata.md) · [Recommendations](./discovery.md) · [Downloads](./downloads.md) |

## The disk is running out of space

For the standard setup, check how much space the folders use and how
much is free:

```sh
sudo du -sh data music
df -h data music
```

If your music or downloads are elsewhere, use their paths on the host.
The per-file download limit caps the size of one download. Total usage
depends on how many tracks you've added. [Make a backup](./operations.md)
before removing data.

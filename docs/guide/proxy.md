# HTTPS with Traefik

Connect Shprotify to an existing Traefik setup. Players will reach the
server over HTTPS on your domain, and Traefik will forward their requests
to Shprotify over a shared Docker network.

## 1. Check your setup

Shprotify should already be running from the [quick start](./quick-start.md).
This example assumes the following Traefik setup:

| What you need | Example value |
| --- | --- |
| A domain pointing to your Traefik server | `music.example.org` |
| A Docker network that Traefik is connected to | `proxy` |
| An HTTPS entry point (`entryPoint`) | `websecure` |
| A configured certificate resolver (`certResolver`) | `letsencrypt` |

Traefik needs its Docker provider enabled. Run `docker network inspect proxy`
to check the network and see which containers are attached.
For help setting up the proxy, see the [Traefik Docker guide](https://doc.traefik.io/traefik/reference/install-configuration/providers/docker/).

## 2. Connect Shprotify to the shared network

This `compose.yaml` builds on the quick start. Replace `music.example.org`
in the `Host` rule with your domain. If your network has a different name,
replace `proxy` in all three places. Use your own entry point and
certificate resolver names if they differ from the example.

<<< ../public/examples/compose.traefik.yaml{yaml}

> [!TIP]
> If you already have a custom `compose.yaml`, copy over the `labels`
> and `networks` sections. Keep your existing folders and settings,
> including your TOML mount if you use one. Remove `ports` to serve
> Shprotify through Traefik.

Shprotify still listens on `0.0.0.0:3000` inside its container.
Traefik reaches that port through the `proxy` network, so you don't need
to publish port 3000 on the host. The labels route the whole domain,
including `/api/v1/` and `/docs`.
[Traefik label reference](https://doc.traefik.io/traefik/reference/routing-configuration/other-providers/docker/)

## 3. Check HTTPS

From the folder containing `compose.yaml`, apply the changes and test
your domain:

```sh
docker compose up -d
curl -fsS https://music.example.org/api/v1/health
```

You should get `{"status":"ok"}`. In your player, use
`https://music.example.org` as the server address. If Swagger is enabled,
you can reach it at `https://music.example.org/docs`.

## If a web player uses another domain {#if-a-web-player-uses-another-domain}

If the player page is at `https://player.example.org`, add that address
to the server's CORS settings. This lets the browser send requests to
Shprotify from the player page.

::: code-group
```yaml [Compose]
# Under environment for the shprotify service
SHPROTIFY_HTTP__CORS_ORIGINS: '["https://player.example.org"]'
```
```toml [TOML]
[http]
cors_origins = ["https://player.example.org"]
```
:::

Include the scheme and any non-default port. Leave out the path and
trailing slash. If you use TOML, add the setting to the existing `[http]`
section. Apply Compose changes with `docker compose up -d`. For TOML-only
changes, use `docker compose restart shprotify`.

> [!NOTE]
> Audio links may contain a temporary `ticket` credential. If Traefik
> logs full request URLs, disable access logs for the Shprotify router
> or configure them to omit credentials and the `Authorization` header.
> [Traefik access logs](https://doc.traefik.io/traefik/observe/logs-and-access-logs/)

## Troubleshooting

| What you see | What to check first |
| --- | --- |
| Traefik returns `404` | Check the `Host` rule, entry point name and `traefik.enable` label. |
| `502 Bad Gateway` | Use `docker network inspect proxy` to find both containers. Check that Shprotify is running and listening on `0.0.0.0:3000`. |
| Certificate error | Check DNS, the `certResolver` name and Traefik's logs. |
| Browser CORS error | Compare the player page's address to `http.cors_origins`, including the scheme and port. |

To read Shprotify's logs, run `docker compose logs --tail 50 shprotify`.

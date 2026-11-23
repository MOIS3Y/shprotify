# Quick start with Docker

Start Shprotify, create an administrator and add your first tracks
to the library. You'll need Docker with Compose and a few audio files.
Run the Docker commands on the computer that will host the server.

## 1. Prepare the folders

```sh
mkdir -p shprotify/data shprotify/music
cd shprotify
```

Put a few tracks in `music/`. Shprotify uses `data/` for its database
and files it adds itself, such as YouTube downloads.

On Linux, give the container user write access to `data/`:

```sh
sudo chown 1000:1000 data
sudo chmod 750 data
```

## 2. Create the Compose file

Save `compose.yaml` in `shprotify/` alongside `data/` and `music/`:

<<< ../public/examples/compose.yaml{yaml}

> [!TIP]
> If you already have a music folder, replace `./music` in
> `./music:/music:ro` with its path. The container user (UID 1000)
> needs read access. The `:ro` suffix keeps the mount read-only.

## 3. Start the server and check it

```sh
docker compose pull
docker compose up -d
curl http://localhost:3000/api/v1/health
```

Once the server is ready, it will respond with:

```json
{"status":"ok"}
```

If you get no response, wait a few seconds and try again.
If the server still doesn't respond, check its logs:

```sh
docker compose logs --tail 50 shprotify
```

::: details Open the server from another computer
The example exposes the port on the server itself. To finish setup
from another computer, run the command below on that computer.
Replace `user@your-server` with your SSH username and server address.

```sh
ssh -N -L 3000:127.0.0.1:3000 user@your-server
```

While the tunnel is open, the `localhost:3000` URLs will also work
on your computer. For permanent access, see [HTTPS with Traefik](./proxy.md).
:::

## 4. Create an administrator

Replace `alice` with your username in all three commands. These create
the user, grant administrator access and set a password.

```sh
docker compose exec shprotify shprotify user create --name alice
docker compose exec shprotify shprotify user role --name alice --role admin
docker compose exec shprotify shprotify user password --name alice
```

The last command asks you to enter the password twice. The terminal
hides the characters as you type.

> [!NOTE]
> The terminal shows an API key once when you create the user.
> Keep it private like a password. For the next step, sign in
> with your username and password.

## 5. Add music to the library

Open <a href="http://localhost:3000/docs" target="_blank" rel="noreferrer">Swagger</a> in your browser.
The steps below show how to start a scan and see what was added.
If your client can manage the library, you can do this from the app.

### Sign in

1. Open **Authentication** → `POST /api/v1/auth/login`.
2. Click **Try it out**. Paste the JSON below into the request box,
   replace the name and password with your own, then click **Execute**.

   ```json
   {
     "name": "alice",
     "password": "your-password"
   }
   ```

3. Copy the value of `access_token` from the response. Click **Authorize**
   at the top of the page, paste the token and confirm.

### Scan your music

1. Under **Admin: Library**, open `POST /api/v1/library/scans` and click
   **Try it out** → **Execute**. Copy the number in the `id` field.
2. In the same section, open `GET /api/v1/library/scans/{id}`, enter
   that number and run the request. If the scan is still running,
   check again in a few seconds. The scan is done when `status`
   is `completed`.
3. Open **Recordings** → `GET /api/v1/recordings` and run the request.
   The imported recordings appear in the `items` list.

If the list is empty or the scan reports errors, check the folder path,
its read permissions and the [scan report](./library.md#import-a-folder).

## What's next

Your server is running and the first tracks are in the library.

- [Connect a player](../clients/apps.md)
- [Add users](./accounts.md)
- [Change server settings](./configuration.md)
- [Back up and update the server](./operations.md)

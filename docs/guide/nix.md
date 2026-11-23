# Install and run with Nix

If you already use Nix, you can run Shprotify directly from GitHub or
build it from a checkout. You can also build a Docker image from either source.

| What you want | Where to start |
| --- | --- |
| Run the server without cloning | [Run the app from GitHub](#from-github) |
| Build or run your own copy of the source | [Work from a checkout](#from-a-checkout) |
| Get an image for Docker Compose | [Build a local image](#docker-image) |

You need the `nix-command` and `flakes` features. If they're off, add
`experimental-features = nix-command flakes` to your `nix.conf`.
[Nix settings](https://nix.dev/manual/nix/latest/command-ref/conf-file.html)

## Prepare a server folder

To run without a container, make a folder for music, settings and data.

```sh
mkdir -p shprotify-server/data shprotify-server/music
cd shprotify-server
```

Save this as `shprotify.toml`. This example makes the server available on
this computer and requires users to sign in to browse the catalog.

```toml
[http]
bind = "127.0.0.1:3000"

[database]
path = "data/shprotify.db"

[library]
directories = ["music"]
public_catalog = false

[docs]
enabled = true
```

Add files to `music/` or point the setting at your existing collection
using an absolute path. Relative paths start from the folder you launch
the server in. The next commands use `shprotify-server/`.

## Run directly from GitHub {#from-github}

Run this command from the server folder:

```sh
nix run 'github:MOIS3Y/shprotify/main#shprotify' -- --config shprotify.toml
```

Nix fetches the flake, prepares the package and runs the `shprotify` app.
Everything after `--` is passed to the server. Replace `main` with a tag
or commit hash to keep using the same source revision.
[Flake references](https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-flake.html)

## Run from a checkout {#from-a-checkout}

From the server folder, clone the repository into a sibling folder.
Run the same app from your local source:

```sh
git clone https://github.com/MOIS3Y/shprotify.git ../shprotify-src
nix run ../shprotify-src#shprotify -- --config shprotify.toml
```

To build the binary first, run these commands from the server folder.
The build leaves a `result` link in the checkout.

```sh
cd ../shprotify-src
nix build .#shprotify
cd ../shprotify-server
../shprotify-src/result/bin/shprotify --config shprotify.toml
```

## Check the server and create an administrator

Open a second terminal in the same `shprotify-server/` folder.
Check that the server is responding:

```sh
curl -fsS http://localhost:3000/api/v1/health
```

For account commands, put `shprotify` on the shell's `PATH`.
Use the same source and version as your running server.

::: code-group
```sh [From GitHub]
nix shell 'github:MOIS3Y/shprotify/main#shprotify'
```
```sh [From a checkout]
nix shell ../shprotify-src#shprotify
```
:::

In the new shell, create the account, grant access and set a password.

```sh
shprotify --config shprotify.toml user create --name alice
shprotify --config shprotify.toml user role --name alice --role admin
shprotify --config shprotify.toml user password --name alice
```

Then [sign in and scan your music](./quick-start.md#sign-in).
The package includes yt-dlp, FFmpeg, Deno, Chromaprint and CA certificates.
Optional integrations are enabled separately in your [settings](./configuration.md).

## Build a Docker image locally {#docker-image}

On Linux, choose a source. For a checkout, run the command from the
folder containing `flake.nix`.

::: code-group
```sh [From GitHub]
nix build 'github:MOIS3Y/shprotify/main#dockerImage' -o result-image
```
```sh [From a checkout]
nix build .#dockerImage -o result-image
```
:::

In that folder, load the archive into Docker:

```sh
docker load -i result-image
```

Docker prints the image name and tag. Nix uses the project version for
the tag. Prepare the folders and `compose.yaml` from the [quick start](./quick-start.md).
Save the loaded tag in a `.env` file next to `compose.yaml`:

```dotenv
SHPROTIFY_IMAGE=ghcr.io/mois3y/shprotify:0.1.0
```

Start Compose with the local image and skip pulling from the registry.

```sh
docker compose up -d --pull never
```

Check that the server responds with `{"status":"ok"}`:

```sh
curl -fsS http://localhost:3000/api/v1/health
```

Then [create an administrator](./quick-start.md#_4-create-an-administrator)
and add music to the catalog.

## Build the docs and run checks

Run these commands from the checkout:

| Command | Result |
| --- | --- |
| `nix develop` | A shell with tools for working on the server and documentation. |
| `nix flake check` | Project checks, including server tests, OpenAPI and the documentation build. |
| `nix build .#docs -o result-docs` | A static site for `/shprotify/`, as used by GitHub Pages. |
| `nix build .#docsRoot -o result-docs-root` | A static site for the root of a domain (`/`). |

Use a simple HTTP server to preview the site built for the root path:

```sh
python3 -m http.server 4173 --directory result-docs-root
```

Open `http://localhost:4173`. For more on the tools and checks,
see [Contributing](../reference/development.md).

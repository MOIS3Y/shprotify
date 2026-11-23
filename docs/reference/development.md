# Contributing

Start with a corrected example, a test for a bug or a small server change.
If you report a problem, include your Shprotify version, what you did,
what you expected and what happened.
[Project issues on GitHub](https://github.com/MOIS3Y/shprotify/issues)

| What you want to change | Where to start |
| --- | --- |
| Server behavior | Run the server locally and find the module. [Architecture](./architecture.md) |
| How data is stored | Review the table relationships and migration workflow. [Database](./database.md) |
| A guide, example or the API reference | Run the website locally. [Documentation and references](./documentation.md) |

## Run the server from source

Clone the repository and enter its directory:

```sh
git clone https://github.com/MOIS3Y/shprotify.git
cd shprotify
```

You'll need Rust 1.99 or newer, Cargo and a C compiler for SQLite.
Install `rustfmt` and Clippy too. If you use Nix, `nix develop`
opens a shell with the development tools. [Nix guide](../guide/nix.md)

```sh
mkdir -p dev/music
test -f dev/shprotify.toml || cp examples/shprotify.toml dev/shprotify.toml
cargo run --locked -- --config dev/shprotify.toml
```

The example creates `dev/shprotify.db`. To work with the catalog, put a few
files in `dev/music/` and set `directories = ["dev/music"]` under the existing
`[library]` section. Set `enabled = true` under `[docs]` if you need Swagger.
Git ignores `dev/`.

Check the server from a second terminal:

```sh
curl -fsS http://localhost:3000/api/v1/health
```

Expect `{"status":"ok"}`. Use the same config with local account commands
so they open the same database:

```sh
cargo run -- --config dev/shprotify.toml user --help
```

## Make and check the change

Keep module tests next to the code they cover. For workflows that cross
several parts of the server, use the HTTP tests in `tests/e2e/`. These start
the real executable with a temporary SQLite database and test audio.
Where needed, local fixtures stand in for external providers. The
scenarios only need loopback sockets.

```sh
cargo test --locked --test e2e
```

Before submitting a change, run these from the repository root:

```sh
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Keep contracts and important constraints next to the Rust code in
`///` and `//!` comments. When user-visible behavior changes, update
the matching pages in both languages. For an API or migration change,
check whether you need to [regenerate a reference](./documentation.md).

## Describe the contribution

In the pull request, explain the problem, what happens after the change
and how you checked it. For a bug fix, include a short way to reproduce
the original behavior when possible.

## CI and images for client development

GitHub Actions runs `nix flake check` on pull requests and pushes to
`main`. The checks build the site, compare the stored OpenAPI with the
server's actual response, and compare the database reference with the
migrations. Every successful push to `main` can deploy the site, including
pushes that change only server code or build inputs.

To test a client against the current development build, use
`ghcr.io/mois3y/shprotify:main`. Set `SHPROTIFY_IMAGE` to that value in
the `.env` next to your Compose file. To pick up changes, run
`docker compose pull shprotify` and `docker compose up -d`. If you need
the same server version throughout testing, use the image tagged
`sha-<full-commit-sha>`. Use a separate data directory for development
images.

Dependabot checks Nix flake inputs and GitHub Actions versions once
a month and proposes updates in pull requests.

## Publishing a release

Set the desired version in `Cargo.toml` and update `Cargo.lock` if the
version changed. With git-cliff installed, preview the release notes from
the repository root using `git cliff --unreleased`.

Tag the checked commit with the matching `vX.Y.Z` and push the tag.
For example, for version `0.1.0` use these commands.

```sh
git tag -a v0.1.0 -m "Shprotify 0.1.0"
git push origin v0.1.0
```

Actions checks the tag against `Cargo.toml`, tests the Linux amd64 image,
pushes the versioned image to GHCR and creates a GitHub release with notes
from git-cliff. The highest stable tag also updates the `latest` image.

### First-time GitHub setup

- In **Settings → Pages**, select **GitHub Actions** as the source.
- After the first successful image push, open **Packages → shprotify →
  Package settings** and set the package visibility to **Public** so
  clients can pull it anonymously. The workflow uses the built-in
  `GITHUB_TOKEN`. If the package already exists and the push is denied,
  grant the repository write access under **Manage Actions access**.
  [GitHub Container registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)

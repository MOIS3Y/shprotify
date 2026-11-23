# Documentation and references

The site is built from Markdown with VitePress. Run a local preview
for prose changes. Regenerate OpenAPI and DBML from the server and
migrations when their contracts change.

## Open the site locally

You'll need Node.js 22 or newer, Python 3 and Graphviz (`dot`).
The `nix develop` shell includes them.

```sh
cd docs
npm ci --ignore-scripts
npm run dev
```

Open the address VitePress prints. After dependencies are installed,
the site checks and builds use local files.

## Find the source file

| What you're editing | Where to look under `docs/` |
| --- | --- |
| Installing and running the server | `guide/` and `ru/guide/` |
| Using and developing clients | `clients/` and `ru/clients/` |
| References and contributing | `reference/` and `ru/reference/` |
| Navigation and theme | `.vitepress/config.ts` and `.vitepress/theme/` |
| Compose and TOML examples for Docker | `public/examples/`. The guides include these files. |

English and Russian articles share the same path under their language
prefixes. Edit a Compose example in its source file so both guides
pick up the change.

## Check the site

```sh
npm run build
npm run preview
```

The build runs `npm run check` for types, matching translations, OpenAPI
and DBML. VitePress checks internal links. Use `preview` to view the
built site under the same base path. For GitHub Pages, build with
`DOCS_BASE=/shprotify/ npm run build`. The default base is `/`.

::: details VitePress dependencies
For VitePress 1.6.4, `package.json` overrides Vite to 6.4.6 for the security
fixes. When upgrading VitePress, check whether the override and local search
fix in `.vitepress/config.ts` can be removed. Run `npm audit` after
dependency changes.
:::

## Update OpenAPI

After editing routes or schemas, build the server from the repository root:

```sh
cargo build --locked
cd docs
npm run api:export
```

The exporter starts a temporary server on loopback with its own database,
reads its schema and writes `docs/public/openapi.json`. Review the diff
and include it with the code change. To check without rewriting the file,
run `npm run api:export -- --check`. For a binary in a different location,
pass `--binary PATH`.

## Update DBML {#update-dbml}

After adding a SQL migration, run these from `docs/`:

```sh
npm run schema:export
npm run check
```

The script applies the migrations to a fresh SQLite database and
updates `docs/database.dbml`. Include the DBML file with the migration.
The site generates the SVG diagrams at build time.
[How to read the database schema](./database.md)

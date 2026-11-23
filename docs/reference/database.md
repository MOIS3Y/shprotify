<script setup>
import { withBase } from 'vitepress'
</script>

# Database schema

The SQL files in `migrations/` define the SQLite schema. To produce the
DBML and diagrams, a script applies the migrations to an empty database.
The documentation can then be checked against the same SQL the server uses.

<a :href="withBase('/database.dbml')" download>Download the complete DBML schema</a>

## Find the area you need

| Area | What it contains |
| --- | --- |
| Catalog | Artists, releases, recordings, audio files, metadata and covers. |
| Personal data | Accounts, credentials, reactions, playlists, queues and history. |
| Discovery | Feeds, suggestions, feedback and links to downloads. |
| Background work | Scans, metadata jobs, downloads and schedules. |

Choose an area. Click the image to open the SVG at full size.
Gray tables show references into other areas.

<SchemaDiagrams />

DBML includes the columns, keys, indexes and constraints of the documented
tables. Partial index conditions appear in notes. SQLx bookkeeping and
FTS5 shadow tables are left out of the diagrams.

## Relationships beyond foreign keys

The diagrams show foreign keys declared in SQLite. Triggers and server
code enforce some relationships:

- A `catalog_metadata` entry can describe an artist, release or recording.
  Catalog triggers maintain that association.
- A temporary audio link depends on the credential that created it.
  The server checks that credential when serving audio.
- Audit records keep historical IDs so you can identify what an edit or
  removal affected.

## Change the schema

Add a SQL file under `migrations/` with the next version number. SQLx
checks the contents of applied migrations against their recorded checksums,
so keep those files intact. Cover the change with a test beside the
database module and [update DBML](./documentation.md#update-dbml).

The SQL also defines triggers, views for client reads and full-text search.
Check the migration if a diagram leaves a write's effects unclear.

::: details Initial SQLite migration
<<< ../../migrations/1_initial_schema.sql{sql}
:::

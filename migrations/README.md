# SQLite migrations

Place SQLx migrations here as `<version>_<description>.sql`.
Versions must be unique and increase with each migration.

Migrations are embedded in the executable during compilation and applied
before HTTP startup. Do not edit or delete a migration after it has been
applied: SQLx validates migration history and checksums on startup.

Each migration runs in a transaction by default. The build script watches
this directory so adding SQL files triggers recompilation.

There are no application tables yet. The first schema migration belongs
with the first library entity; startup currently creates only SQLx's
internal migration history table.

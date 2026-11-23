# SQLite migrations

Place SQLx migrations here as `<version>_<description>.sql`.
Versions must be unique and increase with each migration.

Migrations are embedded in the executable during compilation and applied
before HTTP startup. Do not edit or delete a migration after it has been
applied: SQLx validates migration history and checksums on startup.

Each migration runs in a transaction by default. The build script watches
this directory so adding SQL files triggers recompilation.

`1_initial_schema.sql` creates the complete initial schema, including
constraints, indexes, triggers, full-text search and client read views.
Sections follow the application's functional boundaries.

Add subsequent schema changes as new migrations. Keep schema constraints
and data transformations in SQL and cover their behavior with database
tests beside the relevant Rust module.

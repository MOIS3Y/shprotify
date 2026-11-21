# SQLite migrations

Place SQLx migrations here as `<version>_<description>.sql`.
Versions must be unique and increase with each migration.

Migrations are embedded in the executable during compilation and applied
before HTTP startup. Do not edit or delete a migration after it has been
applied: SQLx validates migration history and checksums on startup.

Each migration runs in a transaction by default. The build script watches
this directory so adding SQL files triggers recompilation.

The first migration creates the shared `artists` catalog. Artist names
are deliberately not unique: distinct artists can share a display name.

The second adds `releases` and ordered `release_artists` credits. Titles
are not unique. Credits reference existing artists; deleting a credited
artist is restricted, while deleting a release removes its credits.

The third adds independent `recordings` and ordered `recording_artists`.
Recordings have no release or audio file dependency. Artist deletion is
restricted while credited; deleting a recording removes its credits.

The fourth adds `release_tracks`: recording occurrences with positive disc
numbers and positions. Slots are unique per release/disc/position, while
recordings can occur at multiple slots or in multiple releases. Release
deletion cascades to tracks; referenced recordings cannot be deleted.

The fifth adds `audio_files` metadata with a globally unique normalized
absolute path and optional positive audio properties. Several files may
represent one recording. Referenced recordings cannot be deleted; file
registration does not access, create, move, or remove physical files.

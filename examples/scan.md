# Read-only collection scan

Set `[library].directories` in a copy of `shprotify.toml`, then run:

```sh
cargo run -- --config dev/shprotify.toml scan > dev/scan.jsonl
```

The command uses the same TOML/environment settings as the server. Relative
roots resolve against the working directory. It reads existing files without
opening SQLite, starting HTTP, or changing the collection or catalog. Running
the server without a subcommand still only validates the configured roots.

Each stdout line is an independent JSON object. Logs go to stderr. Records
arrive in filesystem iteration order; consumers must not depend on ordering.

- `type: "audio"`: `file` contains the absolute `path`, detected `format`,
  source `tags`, and measured `properties`.
- `type: "error"`: `path` and `message` describe an entry that could not be
  read. Other entries are still processed.
- `type: "summary"`: final `audio_files`, `skipped`, and `errors` counts.

An empty roots list succeeds with zero counts. Any reported entry error makes
the final exit status nonzero. Invalid configured roots fail before traversal;
report output failures stop it immediately. Such failures can leave a partial
report without a final summary.

Format detection uses file contents through Lofty, so a WAV named `song.flac`
is reported as `wav`. Unknown content is skipped, but unrecognized content
with a supported audio extension is an error. A readable header and metadata
do not guarantee that the entire audio stream can be decoded.

The scanner selects the primary tag, falling back to the first available tag.
Title, artist, album, album artist, track number, and disc number retain their
source values. Missing tags and unknown properties are `null`; names are not
inferred from filenames, and artist credits are not split into identities.
Duration is in milliseconds, size in bytes, sample rate in hertz, and audio
bitrate in bits per second. Artwork is not loaded.

Configured symlink roots are resolved during validation. Links inside those
roots, including links to files, are skipped along with special files. This
avoids ordinary link loops and traversal outside the roots; it does not
provide isolation against concurrent filesystem replacement. Non-UTF-8 paths,
control characters, and paths over 4096 bytes are reported as errors.

Matching these observations to artists, releases, and recordings, persisting
the results, and launching background scans through HTTP are subsequent work.

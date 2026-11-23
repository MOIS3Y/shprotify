# Offline executable integration tests

Run `cargo test --test e2e` or the complete `cargo test --all-targets` suite.
The same scenarios run in the existing `nix flake check` tests check.

Each scenario starts the real Cargo-built `shprotify` executable and uses
only its CLI and HTTP API. Accounts, database, configuration, generated WAV
and PNG media, and managed storage live in a private temporary directory.
The server binds loopback port zero; the fixture reads its assigned port
from startup logs. Commands, requests and polling have deadlines. Normal
shutdown uses SIGTERM; failure cleanup kills and reaps the server.

Tests clear ambient application configuration, ignore HTTP proxies and
disable external metadata, discovery and automation integrations. Acquisition
uses a generated local process instead of yt-dlp. No Internet connection,
provider credentials, Docker daemon or pre-existing database is required.
Loopback sockets must be available, including inside the Nix build sandbox.

These scenarios complement the tests beside application modules. They check
application workflows, not current availability of upstream music services.

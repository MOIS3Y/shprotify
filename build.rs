//! Rebuild embedded migrations when their files or directory contents change.

/// Watch the migration directory, including newly added SQL files.
fn main() {
    println!("cargo::rerun-if-changed=migrations");
}

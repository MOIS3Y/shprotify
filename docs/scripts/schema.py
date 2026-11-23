"""Derive DBML and local SVG diagrams from the real SQLite migrations."""

import argparse
import hashlib
import html
import re
import sqlite3
import subprocess
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import cast

ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / "docs"


@dataclass(frozen=True)
class Column:
    """One visible SQLite column, including its ordered primary-key slot."""

    name: str
    kind: str
    required: bool
    default: str | None
    primary: int


@dataclass(frozen=True)
class Index:
    """An index with ordered keys and preserved SQLite-specific semantics."""

    name: str
    columns: tuple[str, ...]
    descending: tuple[bool, ...]
    unique: bool
    primary: bool
    predicate: str
    expression: bool


@dataclass(frozen=True)
class ForeignKey:
    """One whole foreign key; composite identities must stay composite."""

    identity: int
    target: str
    columns: tuple[str, ...]
    targets: tuple[str, ...]
    update: str
    delete: str


@dataclass(frozen=True)
class Table:
    """A user-visible table, excluding SQLite and FTS implementation tables."""

    name: str
    kind: str
    sql: str
    columns: list[Column]
    indexes: list[Index]
    references: list[ForeignKey]


def balanced(text: str, opening: int) -> str:
    """Extract parenthesized SQL while respecting quoted literals."""
    depth = 1
    quote = ""
    position = opening + 1
    start = position
    while position < len(text):
        char = text[position]
        if quote:
            if char == quote:
                if position + 1 < len(text) and text[position + 1] == quote:
                    position += 1
                else:
                    quote = ""
        elif char in "'\"":
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return " ".join(text[start:position].split())
        position += 1
    raise ValueError("Unbalanced SQL expression")


def read_indexes(connection: sqlite3.Connection, table: str) -> list[Index]:
    """Read uniqueness, sort order, expression keys and partial filters."""
    rows = cast(
        list[tuple[str, int, str]],
        connection.execute(
            'SELECT name, "unique", origin FROM pragma_index_list(?) '
            + "ORDER BY name",
            (table,),
        ).fetchall(),
    )
    indexes: list[Index] = []
    for name, unique, origin in rows:
        keys = cast(
            list[tuple[str | None, int]],
            connection.execute(
                'SELECT name, "desc" FROM pragma_index_xinfo(?) '
                + 'WHERE "key" = 1 ORDER BY seqno',
                (name,),
            ).fetchall(),
        )
        row = cast(
            tuple[str | None] | None,
            connection.execute(
                "SELECT sql FROM sqlite_schema WHERE type='index' AND name=?",
                (name,),
            ).fetchone(),
        )
        sql = (row[0] if row else None) or ""
        expression = any(column is None for column, _ in keys)
        if expression:
            if len(keys) != 1:
                raise ValueError("Extend exporter for mixed expression keys")
            columns = (balanced(sql, sql.index("(")),)
        else:
            columns = tuple(str(column) for column, _ in keys)
        where = re.search(r"\bWHERE\s+(.+)", sql, re.S | re.I)
        indexes.append(
            Index(
                name,
                columns,
                tuple(bool(desc) for _, desc in keys),
                bool(unique),
                origin == "pk",
                " ".join(where[1].split()) if where else "",
                expression,
            )
        )
    return indexes


def read_table(connection: sqlite3.Connection, name: str, kind: str) -> Table:
    """Inspect columns and group foreign-key rows by their real identity."""
    rows = cast(
        list[tuple[str, str, int, str | None, int]],
        connection.execute(
            'SELECT name, type, "notnull", dflt_value, pk '
            + "FROM pragma_table_xinfo(?) WHERE hidden=0 ORDER BY cid",
            (name,),
        ).fetchall(),
    )
    columns = [
        Column(n, t or "TEXT", bool(nn or pk), d, pk)
        for n, t, nn, d, pk in rows
    ]
    rows_fk = cast(
        list[tuple[int, str, str, str, str, str]],
        connection.execute(
            'SELECT id, "table", "from", "to", on_update, on_delete '
            + "FROM pragma_foreign_key_list(?) ORDER BY id, seq",
            (name,),
        ).fetchall(),
    )
    grouped: defaultdict[int, list[tuple[str, str, str, str, str]]]
    grouped = defaultdict(list)
    for identity, target, source, destination, update, delete in rows_fk:
        grouped[identity].append((target, source, destination, update, delete))
    references = [
        ForeignKey(
            identity,
            parts[0][0],
            tuple(part[1] for part in parts),
            tuple(part[2] for part in parts),
            parts[0][3],
            parts[0][4],
        )
        for identity, parts in grouped.items()
    ]
    sql = cast(
        tuple[str],
        connection.execute(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?",
            (name,),
        ).fetchone(),
    )[0]
    return Table(
        name, kind, sql, columns, read_indexes(connection, name), references
    )


def quoted(value: str) -> str:
    """Quote DBML metadata strings without changing SQL expressions."""
    return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"


def reference_columns(table: str, columns: tuple[str, ...]) -> str:
    """Preserve composite FK ordering in DBML relationship endpoints."""
    names = ", ".join(columns)
    return f"{table}.{names}" if len(columns) == 1 else f"{table}.({names})"


def dbml(tables: list[Table], digest: str) -> str:
    """Export real constraints; notes retain semantics DBML cannot enforce."""
    output = [
        "// Generated by docs/scripts/schema.py; SQL is authoritative.",
        f"// Migration source SHA-256: {digest}",
        "Project shprotify {",
        "  database_type: 'SQLite'",
        "  Note: 'STRICT tables; FTS5 internals and SQLx bookkeeping omitted. "
        + "Views and triggers are documented in the SQL migration.'",
        "}",
        "",
    ]
    notes = {
        "catalog_metadata": "Entity identity is polymorphic; entity_id is "
        + "maintained by catalog triggers, not three separate foreign keys.",
        "catalog_search": "FTS5 virtual table maintained by catalog triggers; "
        + "SQLite-generated shadow tables and hidden columns are omitted.",
        "file_removals": "Recording/file identities deliberately have no FK: "
        + "the audit record must survive file removal.",
        "media_tickets": "parent_digest refers to a key or session; live "
        + "credential validation implements revocation without a SQL FK.",
        "metadata_link_revisions": "Historical identities are audit "
        + "data, not current foreign-key associations.",
    }
    for table in tables:
        output.append(f"Table {table.name} {{")
        primary = tuple(
            c.name
            for c in sorted(
                table.columns,
                key=lambda c: c.primary,
            )
            if c.primary
        )
        for column in table.columns:
            settings: list[str] = []
            if len(primary) == 1 and column.primary:
                settings.append("pk")
                if "AUTOINCREMENT" in table.sql:
                    settings.append("increment")
            if column.required:
                settings.append("not null")
            if column.default is not None:
                settings.append(f"default: `{column.default}`")
            suffix = f" [{', '.join(settings)}]" if settings else ""
            output.append(f'  "{column.name}" {column.kind.lower()}{suffix}')
        indexes: list[str] = []
        for index in table.indexes:
            if index.primary and len(primary) == 1:
                continue
            settings = []
            if index.primary:
                settings.append("pk")
            elif index.unique:
                settings.append("unique")
            if not index.name.startswith("sqlite_autoindex_"):
                settings.append(f"name: {quoted(index.name)}")
            if index.predicate:
                settings.append(
                    f"note: {quoted('SQLite WHERE ' + index.predicate)}"
                )
            names = [
                f"`{name}`" if index.expression else f'"{name}"'
                for name in index.columns
            ]
            names = [
                name + (" [desc]" if desc else "")
                for name, desc in zip(names, index.descending, strict=True)
            ]
            suffix = f" [{', '.join(settings)}]" if settings else ""
            indexes.append(f"    ({', '.join(names)}){suffix}")
        if indexes:
            output.extend(["  indexes {", *indexes, "  }"])
        checks = [
            balanced(table.sql, match.end() - 1)
            for match in re.finditer(r"\bCHECK\s*\(", table.sql, re.I)
        ]
        if checks:
            output.extend(
                ["  checks {", *[f"    `{check}`" for check in checks], "  }"]
            )
        note = notes.get(table.name, "SQLite STRICT table.")
        output.extend([f"  Note: {quoted(note)}", "}", ""])
    for table in tables:
        primary = tuple(
            c.name
            for c in sorted(
                table.columns,
                key=lambda c: c.primary,
            )
            if c.primary
        )
        for reference in table.references:
            source = reference_columns(table.name, reference.columns)
            target = reference_columns(reference.target, reference.targets)
            unique = reference.columns == primary or any(
                index.unique
                and not index.predicate
                and index.columns == reference.columns
                for index in table.indexes
            )
            relation = (
                f"{target} - {source}" if unique else f"{source} > {target}"
            )
            output.append(
                f"Ref fk_{table.name}_{reference.identity}: {relation} "
                + f"[delete: {reference.delete.lower()}, "
                + f"update: {reference.update.lower()}]"
            )
    return "\n".join(output) + "\n"


def group(name: str) -> str:
    """Partition the diagram while retaining cross-boundary FK references."""
    if name in {
        "artists",
        "releases",
        "recordings",
        "release_artists",
        "recording_artists",
        "release_tracks",
        "audio_files",
        "catalog_metadata",
        "catalog_artwork",
        "catalog_search",
    }:
        return "catalog"
    if name == "users" or name.startswith(
        ("user_", "playback_", "playlist", "recording_reaction", "media_")
    ):
        return "personal"
    if name.startswith("discovery_"):
        return "discovery"
    return "processing"


def diagrams(tables: list[Table]) -> None:
    """Render local SVGs from the same introspected constraints."""
    destination = DOCS / "public/diagrams"
    destination.mkdir(parents=True, exist_ok=True)
    all_tables = {table.name: table for table in tables}
    for category in ["catalog", "personal", "discovery", "processing"]:
        selected = {
            table.name for table in tables if group(table.name) == category
        }
        boundary = {
            ref.target
            for table in tables
            if table.name in selected
            for ref in table.references
        } - selected
        lines = [
            "digraph schema {",
            'graph [bgcolor="transparent", rankdir=LR, '
            + 'pad="0.25", nodesep="0.35", ranksep="0.7"];',
            'node [shape=plain, fontname="DejaVu Sans", fontsize=11];',
            'edge [color="#527487", arrowsize=0.6];',
        ]
        for name in sorted(selected | boundary):
            table = all_tables[name]
            keys = {key for ref in table.references for key in ref.columns}
            visible = [
                c
                for c in table.columns
                if c.primary
                or c.name in keys
                or c.name in {"name", "title", "status", "version", "role"}
            ]
            heading = "#d8e8ef" if name in selected else "#edf1f4"
            rows = [
                f'<TR><TD COLSPAN="2" BGCOLOR="{heading}">'
                + f"<B>{name}</B></TD></TR>"
            ]
            for column in visible:
                marker = (
                    "PK"
                    if column.primary
                    else "FK"
                    if column.name in keys
                    else ""
                )
                rows.append(
                    f'<TR><TD ALIGN="LEFT">{marker}</TD>'
                    + f'<TD PORT="{column.name}" ALIGN="LEFT">'
                    + f"{html.escape(column.name)}</TD></TR>"
                )
            if len(visible) < len(table.columns):
                rows.append(
                    '<TR><TD COLSPAN="2"><FONT COLOR="#527487">'
                    + f"+ {len(table.columns) - len(visible)} fields"
                    + "</FONT></TD></TR>"
                )
            lines.append(
                f'"{name}" [label=<<TABLE BORDER="0" CELLBORDER="1" '
                + 'CELLSPACING="0" CELLPADDING="7" COLOR="#b9cdd8" '
                + 'BGCOLOR="#f8fbfd">'
                + "".join(rows)
                + "</TABLE>>];"
            )
        for table in tables:
            if table.name not in selected:
                continue
            for reference in table.references:
                lines.append(f'"{table.name}" -> "{reference.target}";')
        lines.append("}")
        source = destination / f"{category}.dot"
        _ = source.write_text("\n".join(lines) + "\n")
        _ = subprocess.run(
            [
                "dot",
                "-Tsvg",
                str(source),
                "-o",
                str(destination / f"{category}.svg"),
            ],
            check=True,
        )


def main() -> None:
    """Check the tracked DBML before generating disposable diagram assets."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--write", action="store_true")
    _ = parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    paths = sorted(
        (ROOT / "migrations").glob("*.sql"),
        key=lambda path: int(path.name.split("_")[0]),
    )
    source = "\n".join(path.read_text() for path in paths)
    with sqlite3.connect(":memory:") as connection:
        _ = connection.execute("PRAGMA foreign_keys=ON")
        _ = connection.executescript(source)
        rows = cast(
            list[tuple[str, str]],
            connection.execute(
                """SELECT name, type FROM pragma_table_list
                WHERE type IN ('table','virtual')
                  AND name NOT LIKE 'sqlite_%'
                ORDER BY name""",
            ).fetchall(),
        )
        tables = [read_table(connection, name, kind) for name, kind in rows]
    result = dbml(tables, hashlib.sha256(source.encode()).hexdigest())
    target = DOCS / "database.dbml"
    if cast(bool, arguments.write):
        _ = target.write_text(result)
    elif not target.exists() or target.read_text() != result:
        raise SystemExit("DBML is stale; run npm run schema:export in docs")
    diagrams(tables)
    print(
        f"SQLite reference: {len(tables)} tables, "
        + f"{sum(len(table.references) for table in tables)} foreign keys"
    )


if __name__ == "__main__":
    main()

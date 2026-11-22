# Catalog search

```sh
curl --get http://127.0.0.1:3000/api/v1/search \
  --data-urlencode 'q=кино но' --data-urlencode 'limit=20'
curl --get http://127.0.0.1:3000/api/v1/search \
  --data-urlencode 'q=Massive Att' --data-urlencode 'kind=artist'
```

`q` is required: 1–255 Unicode scalar values after trimming, without controls.
`kind` is optional: `artist`, `release`, or `recording`; omitted means all.
`limit` defaults to 50 (1–100); `offset` defaults to 0 (nonnegative). Invalid,
unknown, or repeated query fields return the shared JSON `400` response.

The response contains `items`, `limit`, and `offset`. Each item has `kind`,
`id`, and `title` (the artist name or entity title). IDs are category-scoped;
matching display names do not collapse identities. Fetch complete details
using `/api/v1/artists/{id}`, `/api/v1/releases/{id}`, or
`/api/v1/recordings/{id}` as appropriate. A combined page can mix categories.

Search uses SQLite FTS5 `unicode61` tokenization with case-insensitive Unicode
matching and preserved accents. It matches adjacent ordered tokens, allowing
only the final token to be incomplete: `кино но` matches `Кино ночью`, and
`Massive Att` matches `Massive Attack`. It is not arbitrary substring search:
`assive` does not match `Massive`. Punctuation is handled by the tokenizer;
FTS operators and quotes in user text never become query syntax. Queries that
contain no searchable tokens return an empty page. There is no fuzzy search,
transliteration, or accent removal (`Bjo` does not match `Björk`).

Results use BM25 relevance, with category and numeric ID breaking ties.
Pagination applies to the combined hits and may shift as the catalog changes.
Only catalog names/titles are searched; credits, raw tags, paths, and audio
contents are excluded. Catalog entities without physical files are searchable.

A migration indexes existing rows. SQLite insert/update/delete triggers keep
search entries in the same transaction as catalog changes, including scanner
imports, manual corrections, and rollbacks. Search never reads collection files.

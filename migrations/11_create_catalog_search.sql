-- Unicode token-prefix search over the shared catalog, not physical files.
CREATE VIRTUAL TABLE catalog_search USING fts5(
    kind UNINDEXED, entity_id UNINDEXED, text,
    tokenize = 'unicode61 remove_diacritics 0'
);

INSERT INTO catalog_search (kind, entity_id, text)
    SELECT 'artist', id, name FROM artists;

CREATE TRIGGER artists_search_insert AFTER INSERT ON artists BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('artist', new.id, new.name);
END;
CREATE TRIGGER artists_search_delete AFTER DELETE ON artists BEGIN
    DELETE FROM catalog_search WHERE kind = 'artist' AND entity_id = old.id;
END;
CREATE TRIGGER artists_search_update AFTER UPDATE OF id, name
ON artists BEGIN
    DELETE FROM catalog_search WHERE kind = 'artist' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('artist', new.id, new.name);
END;

INSERT INTO catalog_search (kind, entity_id, text)
    SELECT 'release', id, title FROM releases;

CREATE TRIGGER releases_search_insert AFTER INSERT ON releases BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('release', new.id, new.title);
END;
CREATE TRIGGER releases_search_delete AFTER DELETE ON releases BEGIN
    DELETE FROM catalog_search WHERE kind = 'release' AND entity_id = old.id;
END;
CREATE TRIGGER releases_search_update AFTER UPDATE OF id, title
ON releases BEGIN
    DELETE FROM catalog_search WHERE kind = 'release' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('release', new.id, new.title);
END;

INSERT INTO catalog_search (kind, entity_id, text)
    SELECT 'recording', id, title FROM recordings;

CREATE TRIGGER recordings_search_insert AFTER INSERT ON recordings BEGIN
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('recording', new.id, new.title);
END;
CREATE TRIGGER recordings_search_delete AFTER DELETE ON recordings BEGIN
    DELETE FROM catalog_search WHERE kind = 'recording' AND entity_id = old.id;
END;
CREATE TRIGGER recordings_search_update AFTER UPDATE OF id, title
ON recordings BEGIN
    DELETE FROM catalog_search WHERE kind = 'recording' AND entity_id = old.id;
    INSERT INTO catalog_search (kind, entity_id, text)
        VALUES ('recording', new.id, new.title);
END;

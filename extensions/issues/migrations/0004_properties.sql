-- Issue fields became typed properties (`settings.properties`, typed with
-- tartan.rawkode.academy/types). Stored values keep their JSON: a value of a removed kind
-- name (`text`, `monetary`) is still the same string or integer.
ALTER TABLE issues RENAME COLUMN fields TO properties;

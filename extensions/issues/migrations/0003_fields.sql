-- Field values from the repository's `tartan.cue`, as a JSON object, and a counter
-- that a write can name so two editors cannot overwrite each other.
ALTER TABLE issues ADD COLUMN fields TEXT NOT NULL DEFAULT '{}';
ALTER TABLE issues ADD COLUMN version INTEGER NOT NULL DEFAULT 1;

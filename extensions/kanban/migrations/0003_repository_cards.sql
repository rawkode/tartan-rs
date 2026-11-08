-- Kanban installs on a repository and keeps a card per issue of that
-- repository. A card's values are Issues' typed properties.
ALTER TABLE cards RENAME COLUMN fields TO properties;

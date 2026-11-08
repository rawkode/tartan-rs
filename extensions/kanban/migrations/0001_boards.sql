-- One row per issue in the org, kept from Issues' snapshots: the highest version wins.
CREATE TABLE cards (
	repo_id TEXT NOT NULL,
	repo_path TEXT NOT NULL,
	number INTEGER NOT NULL,
	title TEXT NOT NULL,
	state TEXT NOT NULL,
	project TEXT,
	fields TEXT NOT NULL,
	version INTEGER NOT NULL,
	PRIMARY KEY (repo_id, number)
);

-- A board is a view of the cards: the org's, a repository's or a project's.
CREATE TABLE boards (
	id TEXT PRIMARY KEY,
	name TEXT NOT NULL,
	scope TEXT NOT NULL,
	repo_id TEXT,
	project TEXT,
	field TEXT NOT NULL,
	columns TEXT NOT NULL
);

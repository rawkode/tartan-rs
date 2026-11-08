CREATE TABLE seen (
	source TEXT NOT NULL,
	seq INTEGER NOT NULL,
	type TEXT NOT NULL,
	PRIMARY KEY (source, seq)
);

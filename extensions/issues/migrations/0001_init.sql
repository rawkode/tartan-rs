-- rawkode.issues (repo scope: one database per repository). `number` is the
-- issue's number in its repository, assigned as MAX(number) + 1; issues are
-- never deleted, so numbers are never reused. The author is the actor the
-- host gave the call (`kind`: user, agent, ext or system; `on_behalf_of`: the
-- user an agent acted for).
CREATE TABLE issues (
  number INTEGER PRIMARY KEY CHECK (number > 0),
  title TEXT NOT NULL,
  body TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
  author_kind TEXT NOT NULL,
  author_id TEXT NOT NULL,
  on_behalf_of TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  closed_at INTEGER
);
CREATE INDEX issues_state ON issues(state, number);

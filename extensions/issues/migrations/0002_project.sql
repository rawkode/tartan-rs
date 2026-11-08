-- The project an issue was filed against: the project's root path at the time
-- (`crates/lsp`), or NULL for an issue of the repository as a whole. A root
-- path, not a name: renaming a project (`name:`) keeps its issues; moving its
-- directory leaves them under the old path, since nothing here rewrites them.
ALTER TABLE issues ADD COLUMN project TEXT;
CREATE INDEX issues_project ON issues(project, state, number);

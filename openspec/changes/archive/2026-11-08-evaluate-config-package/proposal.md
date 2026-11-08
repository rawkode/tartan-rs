# Proposal

## Why

The forge reads exactly one file, `tartan.cue`, and evaluates it alone. That is not how CUE works: a configuration is a package — any number of files in a directory, with a module (`cue.mod/`) and imports. Repositories already using CUE (cuenv, for instance) split configuration across files and packages, and must be able to do the same for Tartan.

## What Changes

- The configuration is the CUE package `tartan` in the repository's root directory: every root `*.cue` file that declares `package tartan`, evaluated as CUE evaluates a package directory. Files of other packages in the root are ignored.
- The repository's own `cue.mod/` is used when present: its module path and language version, its local package directories, and imports of packages elsewhere in the module, which are loaded from the same commit. Dependencies on registry modules resolve through the forge's CUE registry configuration.
- A root without any `package tartan` file is the empty configuration.
- Loading is bounded: at most 500 files, 64 directories and 4 MiB of CUE per evaluation; beyond that the configuration is refused.
- The org's configuration repository works the same way.
- Docs and specs stop naming a single `tartan.cue` file.

## Capabilities

### New Capabilities

### Modified Capabilities
- `repo-config`: the configuration is the root CUE package `tartan` (files, module, imports, limits), not one file.
- `config-reconciliation`: a push applies the package at the commit, not one file.

## Impact

- `tartan-config`: evaluates a set of files as a module rooted at the repository root, with the package `tartan` selected.
- `GitHost` lists a directory at a commit (walgit's tree API); the repository actor loads the package's files through it.
- Test Git host, end-to-end tests (multi-file configuration, imports, other packages ignored), and the Configuration docs page.

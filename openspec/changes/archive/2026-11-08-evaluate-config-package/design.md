# Design

## Context

`tartan-config` writes one file plus a schema and a generated `cue.mod/module.cue` to a temporary directory and calls `cuengine::evaluate_cue_package`. cuengine loads with `load.Config{Package: …}` (it already filters instances by package name) and builds a module registry from `CUE_REGISTRY`. walgit serves a file (`/api/blob/{sha}/{path}?raw`) and a directory listing (`/api/tree/{sha}/{path}`) at any commit.

## Goals / Non-Goals

**Goals:**
- Evaluate exactly what `cue eval` would in the repository root for package `tartan`, without cloning the repository.
- Read only the files the package can need.

**Non-Goals:**
- Configuration in directories other than the root (projects' own configuration is a later change).
- Vendoring or caching registry modules in the forge.

## Decisions

- **Load the module, not the repository.** The loader reads the root listing and every root `*.cue` file, then `cue.mod/` (all files, recursively), then follows imports: any import path under the module path maps to a directory in the repository, whose `*.cue` files are loaded and scanned in turn. Other import paths are the standard library or registry modules, left to cuengine. This reads the package's closure instead of every `.cue` file in a monorepo. Alternative: fetch all `*.cue` files — simpler, but costly for large CUE monorepos such as cuenv users'.
- **Find imports by scanning, not parsing.** Import clauses are recognised textually (`import "p"`, `import a "p"`, `import ( … )`, with `:pkg` qualifiers stripped). A missed import shows up as a CUE evaluation error, never as a wrong configuration, so a scanner is safe; a full CUE parser in Rust would be the alternative.
- **The repository's module wins.** With `cue.mod/module.cue`, its module and language version are used. Without it, the forge writes its own module file. The schema is added as one more `package tartan` file in the root under a reserved name.
- **Select the package with cuengine's option.** `evaluate_module` with `package_name: "tartan"` and the root as target, so files of other packages in the root are ignored. A root with no `package tartan` file is detected before evaluating (from the `package` clauses) and is the empty configuration.
- **Bounded loading.** Limits (500 files, 64 directories, 4 MiB) are checked while loading, before anything is evaluated.

## Risks / Trade-offs

- [Registry modules need network access and `CUE_REGISTRY` on forge hosts] → Documented; a configuration whose registry is unreachable is refused, not half-applied.
- [Each push to `main` reads several files from walgit] → Reads are by commit sha and immutable; caching by sha can follow if needed.

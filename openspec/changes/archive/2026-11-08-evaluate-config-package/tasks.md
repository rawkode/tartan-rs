# Tasks

## 1. Evaluating a package

- [x] 1.1 Evaluate a set of files as the root package `tartan` of a module (the repository's `cue.mod/module.cue`, or the forge's), selecting the package with cuengine, and verify with unit tests: several files, another package in the root ignored, an import of a package in the module, no `package tartan` file, and the existing invalid cases
- [x] 1.2 Load a configuration from a source that lists directories and reads files — root `*.cue`, `cue.mod/`, imports under the module path — within 500 files, 64 directories and 4 MiB, and verify with unit tests over an in-memory source, including the limits

## 2. Applying it on push

- [ ] 2.1 Add `list_dir` to `GitHost` for walgit's tree API and the test hosts, load the package through it when `main` moves, and verify the end-to-end tests pass with a configuration split across files, an imported package and a cuenv file in the root
- [x] 2.2 Update the Configuration page in `docs/` (packages, modules, imports, registry, limits) and every mention of a single `tartan.cue`, and verify the docs build and lint

## 3. Verification

- [x] 3.1 Run `scripts/e2e.sh` and verify every suite passes

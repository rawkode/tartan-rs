# Design

## Context

Repository actors already record walgit's ref events (`ref.updated`), and `tartan-config` evaluates a `tartan.cue` with cuengine against the forge's schema. Installations are rows in the org or repository actor's `installations` table, each backed by an extension actor; the install API creates them today. See proposal.md for why configuration moves into the repository.

## Goals / Non-Goals

**Goals:**
- One path to an installation: a push that changes `tartan.cue` on `main`.
- Reconciliation that is safe to repeat and to run concurrently with pushes.
- Org-wide extensions configured from one repository, with per-repository contributions.

**Non-Goals:**
- Multi-file CUE packages or imports in `tartan.cue` (root file only for now).
- Per-extension settings schemas checked in CUE (extensions still validate their own settings).
- Branches other than `main` as the configuration branch.
- Permissions over who may change configuration (identity is not built).

## Decisions

- **Trigger on `ref.updated` for `refs/heads/main`.** The repository actor already records these exactly once. After recording, it reconciles in the background against the latest recorded `main` commit, one reconciliation at a time, so a burst of pushes ends at the newest configuration. Alternative: reconcile in the webhook request — rejected because a slow evaluation would hold up walgit's event cursor.
- **Read `tartan.cue` through the Git host.** walgit serves a file at a commit (`GET /{owner}/{repo}/api/blob/{sha}/{path}?raw`); the `GitHost` trait gains `read_file`. Alternative: clone or fetch into the forge — more plumbing for one file.
- **Validate everything, then apply.** All packages are looked up and their scopes checked before any installation changes, so a bad configuration changes nothing (`config.refused`). Applying runs per installation and is idempotent: the extension actor's spec application compares what it has with what it is given.
- **Uninstall disables.** The installation row and the extension actor's database are kept and the actor refuses calls; naming the extension again re-enables the same installation id. Alternative: destroy the actor — loses data on a mistaken edit.
- **The org's configuration repository is `<org>/tartan`.** walgit refuses names starting with `.`, so `.tartan` is impossible. The repository actor of `tartan` forwards its evaluated configuration to the org actor instead of applying it to itself.
- **Contributions live on the org actor.** Each repository sends its version-less entries after a successful evaluation; the org actor stores them per repository, keeps its last applied configuration, and re-applies it when contributions change, so the org installation's configuration (`settings` plus `repos`) is always current.
- **Kanban reads boards from its configuration.** Org boards come from `boards`; contributed boards from `repos.*.settings.boards`, prefixed with the repository path so ids never collide and scoped to that repository (or its project). Invalid boards are reported by `list_boards` rather than failing the whole configuration, because the forge cannot validate an extension's settings.

## Risks / Trade-offs

- [An infrastructure failure midway through applying leaves some installations changed] → Each step is idempotent and the next push or an actor restart re-applies the latest configuration; the failure is logged.
- [cuengine runs Go in-process with a 10 s timeout; a pathological configuration can use memory until it returns] → Evaluation runs on a blocking thread; size limits on `tartan.cue` can be added if needed.
- [A repository named `tartan` in an org becomes its configuration repository] → Documented; the name is reserved by convention.
- [Removing the install API breaks existing scripts and tests] → Tests move to pushing `tartan.cue`; there are no deployments yet.

## Migration Plan

No deployments exist. Existing installations made through the API are not migrated; they are replaced by the configuration of the next push.

/**
 * The files and commands that configure a new repository: a CUE module importing the types,
 * Issues and Kanban modules the forge serves, a `tartan.cue` installing both extensions, and
 * the shell steps to check it with `cue` and push it. The forge applies exactly what
 * `cue export` of the root package `tartan` gives, so what a person sees locally is what the
 * forge installs.
 */

/** The module versions the example pins. */
export const MODULE_VERSIONS = {
	types: "v0.1.0",
	issues: "v0.6.0",
	kanban: "v0.3.0",
} as const;

/** `cue.mod/module.cue` for a repository. */
export const moduleCue = (org: string, repo: string): string =>
	`module: "tartan.example/${org}/${repo}"
language: version: "v0.14.1"
deps: {
	"tartan.dev/types@v0": v:                     "${MODULE_VERSIONS.types}"
	"github.com/rawkode/tartan-ext/issues@v0": v: "${MODULE_VERSIONS.issues}"
	"github.com/rawkode/tartan-ext/kanban@v0": v: "${MODULE_VERSIONS.kanban}"
}
`;

/** `tartan.cue`: Issues with typed properties and a Kanban board over them. */
export const tartanCue = (): string =>
	`package tartan

import (
	"tartan.dev/types"
	"github.com/rawkode/tartan-ext/issues"
	"github.com/rawkode/tartan-ext/kanban"
)

let props = extensions."rawkode.issues".settings.properties

extensions: "rawkode.issues": issues.#Extension & {
	settings: properties: {
		status: types.#Select & {
			label: "Status"
			options: ["todo", "doing", "review", "done"]
			labels: {
				todo:   "To do"
				doing:  "In progress"
				review: "In review"
				done:   "Done"
			}
			default: "todo"
		}
		priority: types.#Select & {
			label: "Priority"
			options: ["p1", "p2", "p3"]
		}
		tags: types.#MultiSelect & {options: ["bug", "feature", "docs"]}
		assignee: types.#Person
		due:      types.#Date
		estimate: types.#Number & {min: 0, max: 21}
		budget: types.#Money & {currency: "GBP"}
		spec: types.#URL & {label: "Design doc"}
		notes: types.#LongText
	}
}

extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: {
		name:    "Triage"
		columns: props.status
		lanes:   props.priority
		group:   "project"
	}
}
`;

export interface PushCommandsInput {
	cloneUrl: string;
	repo: string;
	registry: string;
	gitToken: string;
}

/** The shell steps from clone to push. */
export const pushCommands = ({ cloneUrl, repo, registry, gitToken }: PushCommandsInput): string =>
	`git clone ${cloneUrl}
cd ${repo}
mkdir -p cue.mod
# write cue.mod/module.cue and tartan.cue (above), then check them:
export CUE_REGISTRY=${registry}
cue export . -e extensions

git add cue.mod tartan.cue
git commit -m "Configure Tartan"
git -c http.extraHeader="Authorization: Bearer ${gitToken}" push origin HEAD:main
`;

/**
 * An in-browser stand-in for the gateway: a `fetch` that answers the gateway's API, Issues'
 * tools and Kanban's tools from memory. It backs the client's tests and the sandbox setting,
 * which lets the interface be explored without a running forge. It follows the same wire
 * contract as the real gateway; it is not a second implementation of the extensions' rules
 * beyond what the interface needs to behave plausibly.
 */

import { isEmpty, optionTitle, validateValue } from "./properties.ts";
import type {
	BoardCell,
	BoardColumn,
	BoardLane,
	BoardView,
	Card,
	CardGroup,
	Installation,
	Issue,
	IssueState,
	PropertyDef,
	PropertyValue,
} from "./types.ts";

/** A board as a repository's Kanban settings define it, after CUE has checked it. */
export interface SandboxBoard {
	id: string;
	name: string;
	project?: string;
	/** A select property and the chosen options, or the issue states. */
	columns: { property: string; values: string[] } | "state";
	/** A property name or `project`. */
	lanes?: string;
	group?: string;
}

export interface SandboxRepo {
	slug: string;
	extensions: Installation[];
	properties: PropertyDef[];
	boards: SandboxBoard[];
	/** Boards whose settings the extension could not use, with why. */
	invalidBoards: { id: string; problem: string }[];
	issues: Issue[];
}

export interface SandboxState {
	orgs: Map<string, { id: string; repos: Map<string, SandboxRepo> }>;
}

const json = (status: number, body: unknown): Response =>
	new Response(body === undefined ? null : JSON.stringify(body), {
		status,
		headers: { "content-type": "application/json" },
	});

const problem = (status: number, error: string, kind?: string): Response =>
	json(status, kind ? { error, kind } : { error });

class Refusal extends Error {
	constructor(readonly status: number, message: string, readonly kind?: string) {
		super(message);
	}
}

const invalid = (message: string) => new Refusal(400, message, "invalid");
const notFound = (message: string) => new Refusal(404, message, "not_found");

const SLUG = /^(?=.{1,64}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

const checkSlug = (slug: unknown): string => {
	if (typeof slug !== "string" || !SLUG.test(slug)) {
		throw new Refusal(
			400,
			`invalid slug ${JSON.stringify(slug)}: use 1 to 64 lowercase letters, digits and -, not at either end`,
		);
	}
	return slug;
};

let counter = 0;
const newId = (prefix: string) => `${prefix}-${(++counter).toString(36).padStart(6, "0")}`;

// ---------------------------------------------------------------------------------------------
// Issues

const snapshot = (issue: Issue): Issue => {
	const { body: _body, ...rest } = issue;
	return { ...rest, properties: { ...issue.properties } };
};

const checkProperties = (repo: SandboxRepo, values: unknown): Record<string, PropertyValue> => {
	if (values === undefined) return {};
	if (typeof values !== "object" || values === null || Array.isArray(values)) {
		throw invalid("properties must be an object");
	}
	const checked: Record<string, PropertyValue> = {};
	for (const [name, value] of Object.entries(values as Record<string, PropertyValue>)) {
		const def = repo.properties.find((d) => d.name === name);
		if (!def) throw invalid(`unknown property ${name}`);
		const why = validateValue(def, value);
		if (why) throw invalid(`${name}: ${why}`);
		checked[name] = value;
	}
	return checked;
};

const issueByNumber = (repo: SandboxRepo, number: unknown): Issue => {
	const issue = repo.issues.find((i) => i.number === number);
	if (!issue) throw notFound(`issue ${String(number)}`);
	return issue;
};

const applyProperties = (issue: Issue, changes: Record<string, PropertyValue>): void => {
	for (const [name, value] of Object.entries(changes)) {
		if (isEmpty(value)) delete issue.properties[name];
		else issue.properties[name] = value;
	}
};

const setState = (issue: Issue, state: IssueState, at: number): boolean => {
	if (issue.state === state) return false;
	issue.state = state;
	issue.closedAt = state === "closed" ? at : null;
	return true;
};

type Input = Record<string, unknown>;

const issuesTool = (repo: SandboxRepo, name: string, input: Input): unknown => {
	const now = Date.now();
	switch (name) {
		case "list": {
			const state = (input.state as string | undefined) ?? "open";
			const limit = (input.limit as number | undefined) ?? 30;
			const project = input.project as string | undefined;
			const before = input.before as number | undefined;
			const inProject = repo.issues.filter((i) => !project || i.project === project);
			const matching = inProject
				.filter((i) => state === "all" || i.state === state)
				.filter((i) => before === undefined || i.number < before)
				.sort((a, b) => b.number - a.number);
			const page = matching.slice(0, limit);
			return {
				state,
				project: project ?? null,
				issues: page.map(snapshot),
				open: inProject.filter((i) => i.state === "open").length,
				closed: inProject.filter((i) => i.state === "closed").length,
				properties: repo.properties,
				nextBefore: matching.length > limit ? page.at(-1)?.number ?? null : null,
			};
		}
		case "get":
			return { issue: { ...issueByNumber(repo, input.number) } };
		case "properties":
			return { properties: repo.properties };
		case "create": {
			const title = typeof input.title === "string" ? input.title.trim() : "";
			if (title.length === 0 || title.length > 200) throw invalid("title must be 1 to 200 characters");
			const values = checkProperties(repo, input.properties);
			const defaults: Record<string, PropertyValue> = {};
			for (const def of repo.properties) {
				if (def.default !== undefined) defaults[def.name] = def.default;
			}
			const issue: Issue = {
				number: repo.issues.reduce((max, i) => Math.max(max, i.number), 0) + 1,
				title,
				project: typeof input.project === "string" ? input.project : null,
				state: "open",
				author: { kind: "user", id: "admin" },
				createdAt: now,
				updatedAt: now,
				closedAt: null,
				properties: {},
				version: 1,
				body: typeof input.body === "string" ? input.body : "",
			};
			applyProperties(issue, { ...defaults, ...values });
			repo.issues.push(issue);
			return { issue: { ...issue } };
		}
		case "update": {
			const issue = issueByNumber(repo, input.number);
			if (input.expectedVersion !== undefined && input.expectedVersion !== issue.version) {
				throw new Refusal(
					409,
					`issue ${issue.number} is at version ${issue.version}, not ${String(input.expectedVersion)}`,
					"conflict",
				);
			}
			const changes = checkProperties(repo, input.properties);
			let changed = false;
			if (typeof input.title === "string" && input.title !== issue.title) {
				if (input.title.trim().length === 0) throw invalid("title must not be empty");
				issue.title = input.title;
				changed = true;
			}
			if (typeof input.body === "string" && input.body !== issue.body) {
				issue.body = input.body;
				changed = true;
			}
			if (Object.keys(changes).length > 0) {
				applyProperties(issue, changes);
				changed = true;
			}
			if (input.state === "open" || input.state === "closed") {
				changed = setState(issue, input.state, now) || changed;
			}
			if (changed) {
				issue.version += 1;
				issue.updatedAt = now;
			}
			return { issue: snapshot(issue), changed };
		}
		case "close":
		case "reopen": {
			const issue = issueByNumber(repo, input.number);
			const changed = setState(issue, name === "close" ? "closed" : "open", now);
			if (changed) {
				issue.version += 1;
				issue.updatedAt = now;
			}
			return { issue: snapshot(issue), changed };
		}
		default:
			throw notFound(`tool ${name} of extension rawkode.issues`);
	}
};

// ---------------------------------------------------------------------------------------------
// Kanban

const toCard = (issue: Issue): Card => ({
	number: issue.number,
	title: issue.title,
	state: issue.state,
	project: issue.project,
	properties: { ...issue.properties },
	version: issue.version,
});

/** A dimension's values for a card: `[null]` when it has none. */
const valuesOf = (issue: Issue, by: string): (string | null)[] => {
	const raw = by === "project" ? issue.project : issue.properties[by];
	if (Array.isArray(raw)) return raw.length > 0 ? raw : [null];
	return isEmpty(raw ?? null) ? [null] : [String(raw)];
};

/** The ordered values of a lane or group dimension, with titles, before "None". */
const dimension = (repo: SandboxRepo, by: string, issues: Issue[]) => {
	const def = repo.properties.find((d) => d.name === by);
	const values = def?.options ??
		[...new Set(issues.flatMap((i) => valuesOf(i, by)).filter((v): v is string => v !== null))]
			.sort();
	return values.map((value) => ({ value, title: def ? optionTitle(def, value) : value }));
};

const renderBoard = (repo: SandboxRepo, board: SandboxBoard): BoardView => {
	const issues = repo.issues.filter((i) => !board.project || i.project === board.project);
	const columnDef = board.columns === "state"
		? undefined
		: repo.properties.find((d) => d.name === (board.columns as { property: string }).property);
	const columns: BoardColumn[] = board.columns === "state"
		? [{ value: "open", title: "Open" }, { value: "closed", title: "Closed" }]
		: board.columns.values.map((value) => ({
			value,
			title: columnDef ? optionTitle(columnDef, value) : value,
		}));
	const columnFor = (issue: Issue): string | null => {
		if (board.columns === "state") return issue.state;
		const raw = issue.properties[board.columns.property];
		if (isEmpty(raw ?? null)) return columns[0]?.value ?? null;
		const value = String(raw);
		return columns.some((c) => c.value === value) ? value : null;
	};
	const placed = issues.filter((i) => columnFor(i) !== null);
	const unplaced = issues.filter((i) => columnFor(i) === null).map(toCard);

	const laneValues: { value: string | null; title: string }[] = board.lanes
		? [...dimension(repo, board.lanes, placed), { value: null, title: "None" }]
		: [{ value: null, title: "" }];
	const lanes: BoardLane[] = laneValues
		.map(({ value, title }): BoardLane => {
			const inLane = board.lanes
				? placed.filter((i) => valuesOf(i, board.lanes!).includes(value))
				: placed;
			const cells: BoardCell[] = columns.map((column) => {
				const inCell = inLane.filter((i) => columnFor(i) === column.value);
				let groups: CardGroup[];
				if (!board.group) {
					groups = [{ value: null, title: "", cards: inCell.map(toCard) }];
				} else {
					const by = board.group;
					groups = [...dimension(repo, by, placed), { value: null, title: "None" }]
						.map((g) => ({
							value: g.value,
							title: g.title,
							cards: inCell.filter((i) => valuesOf(i, by).includes(g.value)).map(toCard),
						}))
						.filter((g) => g.cards.length > 0);
				}
				return { column: column.value, groups };
			});
			return { value, title, cells };
		})
		// The "None" lane appears only when a card has no value for the lanes' property.
		.filter((lane, _index, all) =>
			lane.value !== null || all.length === 1 ||
			lane.cells.some((c) => c.groups.some((g) => g.cards.length > 0))
		);
	return {
		board: {
			id: board.id,
			name: board.name,
			project: board.project ?? null,
			columns,
			lanes: board.lanes ? { by: board.lanes } : null,
			group: board.group ? { by: board.group } : null,
		},
		lanes,
		unplaced,
	};
};

const kanbanTool = (repo: SandboxRepo, name: string, input: Input): unknown => {
	switch (name) {
		case "list_boards":
			return {
				boards: repo.boards.map((b) => ({ id: b.id, name: b.name, project: b.project ?? null })),
				invalid: repo.invalidBoards,
			};
		case "board": {
			const board = repo.boards.find((b) => b.id === input.board);
			if (!board) throw notFound(`board ${String(input.board)}`);
			return renderBoard(repo, board);
		}
		case "move": {
			const board = repo.boards.find((b) => b.id === input.board);
			if (!board) throw notFound(`board ${String(input.board)}`);
			const issue = issueByNumber(repo, input.number);
			if (board.project && issue.project !== board.project) {
				throw notFound(`issue ${issue.number} on board ${board.id}`);
			}
			const column = String(input.column);
			if (board.columns === "state") {
				if (column !== "open" && column !== "closed") throw invalid(`no column ${column}`);
				issuesTool(repo, "update", { number: issue.number, state: column });
			} else {
				if (!board.columns.values.includes(column)) throw invalid(`no column ${column}`);
				issuesTool(repo, "update", {
					number: issue.number,
					properties: { [board.columns.property]: column },
				});
			}
			return { card: toCard(issue) };
		}
		default:
			throw notFound(`tool ${name} of extension rawkode.kanban`);
	}
};

// ---------------------------------------------------------------------------------------------
// Routing

export interface SandboxOptions {
	/** The bearer token requests must carry. */
	token?: string;
	/** Delay before answering, in milliseconds, so loading states show. */
	latencyMs?: number;
	state?: SandboxState;
}

/** A `fetch` answering the gateway's API from memory. */
export const createSandboxFetch = (options: SandboxOptions = {}): typeof fetch => {
	const state = options.state ?? sampleState();
	const token = options.token ?? "dev-admin-token";

	const route = (method: string, path: string, body: Input): Response => {
		const parts = path.replace(/^\/-\/api\/?/, "").split("/").filter(Boolean).map(
			decodeURIComponent,
		);
		const [orgsSeg, org, reposSeg, repo, extSeg, extension, call, name] = parts;
		if (orgsSeg !== "orgs") return problem(404, `${path} not found`);
		if (org === undefined) {
			if (method === "GET") return json(200, [...state.orgs.keys()].sort().map((slug) => ({ slug })));
			const slug = checkSlug(body.slug);
			let entry = state.orgs.get(slug);
			if (!entry) {
				entry = { id: newId("org"), repos: new Map() };
				state.orgs.set(slug, entry);
			}
			return json(201, { id: entry.id, slug, pool: null });
		}
		checkSlug(org);
		const orgEntry = state.orgs.get(org);
		if (!orgEntry) throw new Refusal(404, `org ${org} not found`);
		if (reposSeg === undefined) return json(200, { id: orgEntry.id, slug: org, pool: null });
		if (reposSeg !== "repos") return problem(404, `${path} not found`);
		const cloneUrl = (slug: string) => `http://127.0.0.1:9200/${org}/${slug}.git`;
		if (repo === undefined) {
			if (method === "GET") {
				return json(
					200,
					[...orgEntry.repos.keys()].sort().map((slug) => ({
						slug,
						path: `${org}/${slug}`,
						clone_url: cloneUrl(slug),
					})),
				);
			}
			const slug = checkSlug(body.slug);
			if (!orgEntry.repos.has(slug)) {
				orgEntry.repos.set(slug, {
					slug,
					extensions: [],
					properties: [],
					boards: [],
					invalidBoards: [],
					issues: [],
				});
			}
			return json(201, {
				id: `${orgEntry.id}/${slug}`,
				org_id: orgEntry.id,
				org_slug: org,
				slug,
				pool: null,
				clone_url: cloneUrl(slug),
			});
		}
		checkSlug(repo);
		const repoEntry = orgEntry.repos.get(repo);
		if (!repoEntry) throw new Refusal(404, `repository ${org}/${repo} not found`);
		if (extSeg === undefined) {
			return json(200, {
				id: `${orgEntry.id}/${repo}`,
				org_id: orgEntry.id,
				org_slug: org,
				slug: repo,
				pool: null,
				clone_url: cloneUrl(repo),
				hosted_in: null,
			});
		}
		if (extSeg !== "extensions") return problem(404, `${path} not found`);
		if (extension === undefined) return json(200, repoEntry.extensions);
		if (!repoEntry.extensions.some((e) => e.extension_id === extension)) {
			throw new Refusal(404, `extension ${extension} on ${org}/${repo} not found`);
		}
		if (call !== "tools" || name === undefined) {
			throw new Refusal(404, `${call ?? ""} of extension ${extension} not found`);
		}
		if (extension === "rawkode.issues") return json(200, issuesTool(repoEntry, name, body));
		if (extension === "rawkode.kanban") return json(200, kanbanTool(repoEntry, name, body));
		throw notFound(`tool ${name} of extension ${extension}`);
	};

	return async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
		if (options.latencyMs) await new Promise((r) => setTimeout(r, options.latencyMs));
		const url = new URL(
			typeof input === "string" ? input : input instanceof URL ? input.href : input.url,
			"http://sandbox.invalid",
		);
		const method = (init?.method ?? "GET").toUpperCase();
		const headers = new Headers(init?.headers);
		if (headers.get("authorization") !== `Bearer ${token}`) {
			return problem(401, "unauthorized");
		}
		let body: Input = {};
		if (typeof init?.body === "string" && init.body.length > 0) {
			try {
				body = JSON.parse(init.body) as Input;
			} catch {
				return problem(400, "the body is not JSON");
			}
		}
		try {
			return route(method, url.pathname, body);
		} catch (error) {
			if (error instanceof Refusal) return problem(error.status, error.message, error.kind);
			throw error;
		}
	};
};

// ---------------------------------------------------------------------------------------------
// Sample data

const DAY = 86_400_000;

/** An org `acme` with a repository `api` that installs Issues and Kanban, and some issues. */
export const sampleState = (): SandboxState => {
	const properties: PropertyDef[] = [
		{
			name: "status",
			kind: "select",
			label: "Status",
			options: ["todo", "doing", "review", "done"],
			labels: { todo: "To do", doing: "In progress", review: "In review", done: "Done" },
			default: "todo",
		},
		{
			name: "priority",
			kind: "select",
			label: "Priority",
			options: ["p1", "p2", "p3"],
			labels: { p1: "P1 · urgent", p2: "P2 · soon", p3: "P3 · later" },
		},
		{
			name: "tags",
			kind: "multiselect",
			label: "Tags",
			options: ["bug", "feature", "docs", "infra"],
		},
		{ name: "assignee", kind: "person", label: "Assignee" },
		{ name: "due", kind: "date", label: "Due" },
		{ name: "estimate", kind: "number", label: "Estimate (points)", min: 0, max: 21 },
		{ name: "budget", kind: "money", label: "Budget", currency: "GBP" },
		{ name: "blocked", kind: "boolean", label: "Blocked", default: false },
		{ name: "link", kind: "url", label: "Design doc" },
		{ name: "summary", kind: "shorttext", label: "Summary", maxLength: 120 },
		{ name: "notes", kind: "longtext", label: "Notes" },
	];
	const base = Date.now() - 20 * DAY;
	const seed: [string, string | null, Record<string, PropertyValue>, IssueState?][] = [
		["Accept pushes over HTTP/2", "crates/gateway", {
			status: "doing",
			priority: "p1",
			tags: ["feature", "infra"],
			assignee: "admin",
			estimate: 5,
		}],
		["Board columns drop cards without a status", "extensions/kanban", {
			status: "todo",
			priority: "p1",
			tags: ["bug"],
			due: new Date(Date.now() + 3 * DAY).toISOString().slice(0, 10),
		}],
		["Document the forge configuration file", "docs", {
			status: "review",
			priority: "p2",
			tags: ["docs"],
			assignee: "rawkode",
			link: "https://example.com/design/forge-config",
		}],
		["Render long text properties as Markdown", "extensions/issues", {
			status: "done",
			priority: "p2",
			tags: ["feature"],
			notes: "Use **CommonMark**, sanitised.\n\n- headings\n- lists\n- `code`",
		}],
		["Budget the registry storage", null, {
			status: "todo",
			priority: "p3",
			budget: 125000,
			blocked: true,
		}],
		["Retry webhook deliveries with backoff", "crates/gateway", {
			status: "doing",
			tags: ["infra"],
			estimate: 3,
		}],
		["Archive the old org boards", null, { status: "wontfix", priority: "p3" }],
		["Ship the web interface", "web", {
			status: "done",
			priority: "p1",
			tags: ["feature"],
			assignee: "admin",
		}, "closed"],
	];
	const issues: Issue[] = seed.map(([title, project, props, state], index) => ({
		number: index + 1,
		title,
		project,
		state: state ?? "open",
		author: { kind: "user", id: index % 2 === 0 ? "admin" : "rawkode" },
		createdAt: base + index * DAY,
		updatedAt: base + index * DAY + 3_600_000,
		closedAt: state === "closed" ? base + index * DAY + 7_200_000 : null,
		properties: props,
		version: 1,
		body: index === 0
			? "Clients negotiating `h2` fall back to HTTP/1.1 today.\n\n## Acceptance\n\n1. `git push` over h2 succeeds\n2. The e2e suite covers it"
			: "",
	}));
	const repo: SandboxRepo = {
		slug: "api",
		extensions: [
			{
				installation_id: "inst-issues",
				extension_id: "rawkode.issues",
				version: "0.6.0",
				digest: "sha256:5b1f0c7e4a9d3e8f2a6c1b7d9e0f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f",
			},
			{
				installation_id: "inst-kanban",
				extension_id: "rawkode.kanban",
				version: "0.3.0",
				digest: "sha256:9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d",
			},
		],
		properties,
		boards: [
			{
				id: "triage",
				name: "Triage",
				columns: { property: "status", values: ["todo", "doing", "review", "done"] },
				lanes: "priority",
				group: "project",
			},
			{
				id: "gateway",
				name: "Gateway",
				project: "crates/gateway",
				columns: { property: "status", values: ["todo", "doing", "done"] },
			},
			{ id: "open-closed", name: "Open and closed", columns: "state", group: "tags" },
		],
		invalidBoards: [],
		issues,
	};
	const repos = new Map<string, SandboxRepo>([["api", repo]]);
	return { orgs: new Map([["acme", { id: "org-acme", repos }]]) };
};

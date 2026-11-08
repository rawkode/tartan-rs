/**
 * The shapes the gateway and the first-party extensions exchange. Field names follow the
 * wire format exactly: the gateway serialises Rust structs in snake_case, while extension
 * tools answer in camelCase.
 */

/** An org in the forge's list of orgs (`GET /-/api/orgs`). */
export interface OrgSummary {
	slug: string;
}

/** An org and where its actor runs (`POST /-/api/orgs`, `GET /-/api/orgs/{org}`). */
export interface OrgStatus {
	id: string;
	slug: string;
	/** The Rivet pool hosting the org actor; `null` is the default pool. */
	pool: string | null;
}

/** A repository in its org's list (`GET /-/api/orgs/{org}/repos`). */
export interface RepoSummary {
	slug: string;
	/** `org/repo`. */
	path: string;
	clone_url: string;
}

/** A repository as the org knows it (`POST /-/api/orgs/{org}/repos`). */
export interface RepoInfo {
	id: string;
	org_id: string;
	org_slug: string;
	slug: string;
	pool: string | null;
	clone_url: string;
}

/** A repository and where its actor runs (`GET /-/api/orgs/{org}/repos/{repo}`). */
export interface RepoView extends RepoInfo {
	hosted_in: string | null;
}

/** An extension installed on a repository by its committed CUE configuration. */
export interface Installation {
	installation_id: string;
	extension_id: string;
	version: string;
	digest: string;
}

/** What the gateway answers when a request fails. */
export interface ProblemBody {
	error: string;
	/** Set when an extension refused or failed a call. */
	kind?: ExtensionErrorKind;
}

/** The error kinds an extension call can fail with. */
export type ExtensionErrorKind =
	| "denied"
	| "not_found"
	| "invalid"
	| "conflict"
	| "unavailable"
	| "internal";

/** The ways to call an extension through the gateway. */
export type CallKind = "tools" | "render" | "actions";

// ---------------------------------------------------------------------------------------------
// tartan.dev/types and Issues

/** Every property kind `tartan.dev/types` defines. */
export const PROPERTY_KINDS = [
	"shorttext",
	"longtext",
	"number",
	"date",
	"boolean",
	"select",
	"multiselect",
	"person",
	"url",
	"money",
] as const;

export type PropertyKind = (typeof PROPERTY_KINDS)[number];

/** A property value as Issues stores it: by kind, a string, number, boolean or list. */
export type PropertyValue = string | number | boolean | string[] | null;

/** A property definition, as Issues' `properties` tool describes it. */
export interface PropertyDef {
	name: string;
	kind: PropertyKind;
	label?: string;
	/** `select` and `multiselect`: the options, in order. */
	options?: string[];
	/** `select` and `multiselect`: titles for some options. */
	labels?: Record<string, string>;
	default?: Exclude<PropertyValue, null>;
	/** `number`. */
	min?: number;
	max?: number;
	/** `shorttext` and `longtext`. */
	maxLength?: number;
	/** `money`: an ISO 4217 code; values are integers in its minor units. */
	currency?: string;
}

/** Who did something: a user or an agent, possibly on someone's behalf. */
export interface Principal {
	kind: string;
	id: string;
	onBehalfOf?: string;
}

export type IssueState = "open" | "closed";

/** An issue. `body` is present on `get` and `create`, absent from lists. */
export interface Issue {
	number: number;
	title: string;
	project: string | null;
	state: IssueState;
	author: Principal;
	/** Unix milliseconds. */
	createdAt: number;
	updatedAt: number;
	closedAt: number | null;
	properties: Record<string, PropertyValue>;
	version: number;
	body?: string;
}

export interface IssueListInput {
	state?: IssueState | "all";
	limit?: number;
	project?: string;
	before?: number;
}

export interface IssueListOutput {
	state: IssueState | "all";
	project: string | null;
	issues: Issue[];
	open: number;
	closed: number;
	nextBefore: number | null;
}

export interface IssueCreateInput {
	title: string;
	body?: string;
	project?: string;
	properties?: Record<string, PropertyValue>;
}

export interface IssueUpdateInput {
	number: number;
	title?: string;
	body?: string;
	properties?: Record<string, PropertyValue>;
	state?: IssueState;
	expectedVersion?: number;
	project?: string;
}

export interface IssueChange {
	issue: Issue;
	changed: boolean;
}

// ---------------------------------------------------------------------------------------------
// Kanban

export interface BoardSummary {
	id: string;
	name: string;
	project: string | null;
}

export interface InvalidBoard {
	id: string;
	problem: string;
}

export interface BoardList {
	boards: BoardSummary[];
	invalid: InvalidBoard[];
}

export interface Card {
	number: number;
	title: string;
	state: IssueState;
	project: string | null;
	properties: Record<string, PropertyValue>;
	version: number;
}

export interface BoardColumn {
	value: string;
	title: string;
}

export interface CardGroup {
	/** `null` is the final "None" group, or the only group when the board has no grouping. */
	value: string | null;
	title: string;
	cards: Card[];
}

export interface BoardCell {
	column: string;
	groups: CardGroup[];
}

export interface BoardLane {
	/** `null` is the final "None" lane, or the only lane when the board has no lanes. */
	value: string | null;
	title: string;
	cells: BoardCell[];
}

export interface BoardMeta {
	id: string;
	name: string;
	project: string | null;
	columns: BoardColumn[];
	lanes: { by: string } | null;
	group: { by: string } | null;
}

export interface BoardView {
	board: BoardMeta;
	lanes: BoardLane[];
	unplaced: Card[];
}

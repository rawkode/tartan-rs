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
// tartan.rawkode.academy/types and Issues

/** Every property kind `tartan.rawkode.academy/types` defines. */
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

/**
 * Who did something: a person (`kind: "user"`, `id` their DID), or an agent (`kind: "agent"`,
 * `id` `token:<id>`) on behalf of a person's DID. Issues filed before persons were DIDs keep
 * their earlier ids, such as `admin`.
 */
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
	/** The repository's property definitions, as the `properties` tool gives them. */
	properties: PropertyDef[];
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
	/** For a person property: the DID the group is keyed by, shown by handle. */
	person?: string;
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
	/** For a person property: the DID the lane is keyed by, shown by handle. */
	person?: string;
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

// ---------------------------------------------------------------------------------------------
// Identity: the caller, people, sessions and access tokens

/** A right on the forge (design D4): what a caller may do. */
export type Right = "read" | "write" | "admin";

/**
 * `GET /-/api/me`: who the caller is. `rights` is empty for a signed-in person the forge's
 * `readers` setting does not admit; `dev_identity` says the forge runs its development
 * identity network, so the sign-in page offers the devnet shortcut.
 */
export type Me =
	| { anonymous: true; dev_identity: boolean }
	| {
		anonymous?: false;
		did: string;
		handle: string | null;
		owner: boolean;
		/** `browser` or `cli` for a session, `token` for an access token. */
		kind: string;
		rights: Right[];
		dev_identity: boolean;
	};

/** A signed-in caller. */
export type SignedInMe = Extract<Me, { did: string }>;

/** What `GET /-/api/people` knows of a DID: its verified handle (or `handle.invalid`). */
export interface PersonInfo {
	handle: string | null;
	status: string;
}

/** A browser or CLI session of the caller (`GET /-/api/sessions`). */
export interface SessionInfo {
	id: string;
	kind: "browser" | "cli";
	device: string | null;
	created_at: number;
	last_used_at: number;
	expires_at: number;
	/** Whether this is the session making the request. */
	current: boolean;
}

/** An access token as listed: never its secret. */
export interface TokenInfo {
	id: string;
	did: string;
	name: string;
	scopes: Right[];
	repos: string[] | null;
	created_at: number;
	expires_at: number;
	last_used_at: number | null;
	revoked_at: number | null;
	revoked_by: string | null;
}

export interface TokenRequest {
	name: string;
	scopes: Right[];
	repos: string[] | null;
	expires_in_days: number;
}

/** A created token: the only answer that carries its `tartan_pat_` secret. */
export interface NewToken {
	token: TokenInfo;
	secret: string;
}

// ---------------------------------------------------------------------------------------------
// Commit signatures (design D15, *Answer shapes*)

/** A stored signature result's status. */
export type SignatureStatus =
	| "verified"
	| "unsigned"
	| "bad_signature"
	| "signer_inactive"
	| "unsupported_format"
	| "unverifiable"
	| "not_checked";

export interface GitPerson {
	name: string;
	email: string;
}

/** A commit's stored signature result. */
export interface StoredSignature {
	status: SignatureStatus;
	oid: string;
	detail: string | null;
	/** The signer's DID: verified for `verified`, as the signature names it otherwise. */
	did: string | null;
	rkey: string | null;
	record_cid: string | null;
	/** The `at://` URI of the signature record. */
	record_uri: string | null;
	/** The signer's repository revision that proved the record. */
	record_rev: string | null;
	verified_at: number | null;
	revoked_at: number | null;
	revoked_reason: "withdrawn" | "deleted" | "replaced" | null;
	did_deactivated_at: number | null;
	/** The signer's account status when `did_deactivated_at` is set, such as `deleted`. */
	did_status: string | null;
	/** True when the signature record was never found. */
	record_missing: boolean;
	author: GitPerson | null;
	committer: GitPerson | null;
	proof_url: string | null;
	annotations: string[];
}

/** A check still queued. */
export interface PendingSignature {
	status: "pending";
	attempts: number;
	next_at: number | null;
	last_error: string | null;
	did: string | null;
	/** True while the check waits for the signer to publish the record. */
	record_missing: boolean;
}

export type CommitSignature = StoredSignature | PendingSignature;

/** A commit of a ref update that keeps the ref from complying with the signing rule. */
export interface SigningViolation {
	oid: string;
	problem: string;
	did: string | null;
	/** Whether the commit's signer is one the rule allows. */
	allowed: boolean;
}

/** A ref's head and how its latest update stands against the signing rule. */
export type RefSignature = CommitSignature & {
	ref: string;
	oid: string;
	/** Whether the signing rule covers the ref. */
	covered: boolean;
	/** `null` while pending or when the rule does not cover the ref. */
	compliant: boolean | null;
	violations: SigningViolation[];
};

/** `GET …/config/status`: the applied configuration commit and any pending one. */
export interface ConfigStatus {
	applied: { commit: string } | null;
	pending: { commit: string; waiting_on: "signature_check" | "signing_violation" | "evaluation" } | null;
}

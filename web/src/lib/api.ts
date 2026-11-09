/**
 * A typed client for the Tartan gateway's HTTP API, the sign-in routes the page itself calls,
 * and the first-party extensions' tools, which the gateway exposes at
 * `POST /-/api/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}`.
 *
 * Requests carry the browser's session cookie (`credentials: "same-origin"`) and never an
 * `Authorization` header. Failures become `ApiError`s that keep the gateway's problem body
 * (`{error, kind?}`) so views can show exactly what the forge said. A 401 calls
 * `onUnauthorized`, so the page can send the person to sign in; a 403 from a call that needs a
 * right the caller lacks (design D15, *Read-only presentation*) is explained as
 * [`READ_ONLY_MESSAGE`], while any other 403, such as the `Origin` check's, keeps the server's
 * text.
 */

import type {
	BoardList,
	BoardView,
	CallKind,
	Card,
	CommitSignature,
	ConfigStatus,
	Installation,
	Issue,
	IssueChange,
	IssueCreateInput,
	IssueListInput,
	IssueListOutput,
	IssueUpdateInput,
	Me,
	NewToken,
	OrgStatus,
	OrgSummary,
	PersonInfo,
	ProblemBody,
	PropertyDef,
	RefSignature,
	RepoInfo,
	RepoSummary,
	RepoView,
	Right,
	SessionInfo,
	TokenInfo,
	TokenRequest,
} from "./types.ts";

/** The id Issues installs under. */
export const ISSUES_ID = "issues.tartan.rawkode.academy";
/** The id Kanban installs under. */
export const KANBAN_ID = "kanban.tartan.rawkode.academy";

/** What a write refused for lack of a right says (design D15, *Read-only presentation*). */
export const READ_ONLY_MESSAGE = "Read-only: only forge owners can make changes until membership exists.";

/** The most DIDs one `GET /-/api/people` asks about. */
export const PEOPLE_BATCH = 100;

/** A failed request: the HTTP status (0 when the gateway was unreachable) and its problem. */
export class ApiError extends Error {
	readonly status: number;
	readonly problem: ProblemBody | null;
	readonly method: string;
	readonly path: string;

	constructor(
		status: number,
		problem: ProblemBody | null,
		message: string,
		method: string,
		path: string,
	) {
		super(message);
		this.name = "ApiError";
		this.status = status;
		this.problem = problem;
		this.method = method;
		this.path = path;
	}

	/** The extension error kind, when an extension refused the call. */
	get kind(): string | undefined {
		return this.problem?.kind;
	}
}

export interface ClientOptions {
	/** Prefixed to every path; empty means the page's own origin. */
	baseUrl?: string;
	fetch?: typeof globalThis.fetch;
	/**
	 * The caller's rights from `GET /-/api/me`, or `null` while unknown. A 403 from a call that
	 * needs a right missing here is explained as [`READ_ONLY_MESSAGE`].
	 */
	rights?: () => readonly Right[] | null;
	/** Called on every 401, before the request fails. */
	onUnauthorized?: () => void;
}

type Method = "GET" | "POST" | "DELETE";

const isProblem = (value: unknown): value is ProblemBody =>
	typeof value === "object" && value !== null &&
	typeof (value as { error?: unknown }).error === "string";

const segment = (value: string): string => encodeURIComponent(value);

/** Creates a client for one gateway. */
export const createClient = (options: ClientOptions) => {
	const baseUrl = (options.baseUrl ?? "").replace(/\/+$/, "");
	const doFetch = options.fetch ?? ((...args: Parameters<typeof fetch>) => fetch(...args));

	const request = async <T>(
		method: Method,
		path: string,
		body?: unknown,
		signal?: AbortSignal,
		need?: Right,
	): Promise<T> => {
		const headers: Record<string, string> = { accept: "application/json" };
		const init: RequestInit = { method, headers, credentials: "same-origin" };
		if (body !== undefined) {
			headers["content-type"] = "application/json";
			init.body = JSON.stringify(body);
		}
		if (signal) init.signal = signal;
		let response: Response;
		try {
			response = await doFetch(`${baseUrl}${path}`, init);
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
			throw new ApiError(
				0,
				null,
				"Could not reach the gateway. Is it running, and is the dev server's proxy pointing at it?",
				method,
				path,
			);
		}
		const text = await response.text();
		let parsed: unknown = undefined;
		if (text.length > 0) {
			try {
				parsed = JSON.parse(text);
			} catch {
				parsed = undefined;
			}
		}
		if (!response.ok) {
			if (response.status === 401) options.onUnauthorized?.();
			const problem = isProblem(parsed) ? parsed : null;
			const rights = options.rights?.() ?? null;
			const lacksRight = response.status === 403 && need !== undefined && rights !== null &&
				!rights.includes(need);
			const message = lacksRight ? READ_ONLY_MESSAGE : problem?.error ??
				(response.status >= 500 && text.length === 0
					? "The gateway did not answer (is it running?)"
					: text.trim() || response.statusText || `HTTP ${response.status}`);
			throw new ApiError(response.status, problem, message, method, path);
		}
		return parsed as T;
	};

	const repoPath = (org: string, repo: string) => `/-/api/orgs/${segment(org)}/repos/${segment(repo)}`;

	/** Calls an extension installed on a repository: tools and actions need `write`. */
	const call = <T>(
		org: string,
		repo: string,
		extension: string,
		kind: CallKind,
		name: string,
		input: unknown = {},
		signal?: AbortSignal,
	): Promise<T> =>
		request<T>(
			"POST",
			`${repoPath(org, repo)}/extensions/${segment(extension)}/${kind}/${segment(name)}`,
			input,
			signal,
			kind === "render" ? "read" : "write",
		);

	const commitPath = (org: string, repo: string, sha: string) =>
		`${repoPath(org, repo)}/commits/${segment(sha)}/signature`;

	/** Issues' tools on one repository. */
	const issues = (org: string, repo: string) => {
		const tool = <T>(name: string, input: unknown, signal?: AbortSignal) =>
			call<T>(org, repo, ISSUES_ID, "tools", name, input, signal);
		return {
			list: (input: IssueListInput = {}, signal?: AbortSignal) =>
				tool<IssueListOutput>("list", input, signal),
			get: (number: number, signal?: AbortSignal) =>
				tool<{ issue: Issue }>("get", { number }, signal).then((out) => out.issue),
			create: (input: IssueCreateInput) =>
				tool<{ issue: Issue }>("create", input).then((out) => out.issue),
			update: (input: IssueUpdateInput) => tool<IssueChange>("update", input),
			close: (number: number) => tool<IssueChange>("close", { number }),
			reopen: (number: number) => tool<IssueChange>("reopen", { number }),
			properties: (signal?: AbortSignal) =>
				tool<{ properties: PropertyDef[] }>("properties", {}, signal).then((out) =>
					out.properties
				),
		};
	};

	/** Kanban's tools on one repository. */
	const kanban = (org: string, repo: string) => {
		const tool = <T>(name: string, input: unknown, signal?: AbortSignal) =>
			call<T>(org, repo, KANBAN_ID, "tools", name, input, signal);
		return {
			listBoards: (signal?: AbortSignal) => tool<BoardList>("list_boards", {}, signal),
			board: (board: string, signal?: AbortSignal) => tool<BoardView>("board", { board }, signal),
			move: (board: string, number: number, column: string) =>
				tool<{ card: Card }>("move", { board, number, column }).then((out) => out.card),
		};
	};

	return {
		/** Who the caller is; anonymous callers get `{anonymous: true}`, not a 401. */
		me: (signal?: AbortSignal) => request<Me>("GET", "/-/api/me", undefined, signal),
		/** Handles for up to [`PEOPLE_BATCH`] DIDs; a DID the forge never resolved maps to null. */
		people: (dids: readonly string[], signal?: AbortSignal) =>
			request<Record<string, PersonInfo | null>>(
				"GET",
				`/-/api/people?${dids.map((did) => `did=${encodeURIComponent(did)}`).join("&")}`,
				undefined,
				signal,
			),
		/** A handle's DID, verified both ways by the forge. */
		resolveHandle: (handle: string) =>
			request<{ did: string; handle: string }>("POST", "/-/api/people/resolve", { handle }),
		sessions: (signal?: AbortSignal) => request<SessionInfo[]>("GET", "/-/api/sessions", undefined, signal),
		endSession: (id: string) => request<void>("DELETE", `/-/api/sessions/${segment(id)}`),
		tokens: (signal?: AbortSignal) => request<TokenInfo[]>("GET", "/-/api/tokens", undefined, signal),
		createToken: (token: TokenRequest) => request<NewToken>("POST", "/-/api/tokens", token),
		revokeToken: (id: string) => request<void>("DELETE", `/-/api/tokens/${segment(id)}`),
		/** Ends the browser's session and clears its cookie. */
		signOut: () => request<void>("POST", "/-/auth/logout"),
		/**
		 * Reads a sign-in failure's details once: only the browser that ran the flow can, and only
		 * once. Anything else answers 404.
		 */
		outcome: (id: string, code: string) =>
			request<Record<string, string>>("POST", "/-/auth/outcome", { id, code }),
		/** Approves `tt login`: answers the loopback address that hands `tt` its one-time code. */
		approveCli: (approval: { port: number; state: string; challenge: string; device: string }) =>
			request<{ redirect: string }>("POST", "/-/auth/cli/approve", approval),
		commitSignature: (org: string, repo: string, sha: string, signal?: AbortSignal) =>
			request<CommitSignature>("GET", commitPath(org, repo, sha), undefined, signal),
		/** Queues an `unverifiable` result again: an owner's or its signer's re-check. */
		recheckSignature: (org: string, repo: string, sha: string) =>
			request<void>("POST", `${commitPath(org, repo, sha)}/recheck`),
		/** Every branch head with its signature result and compliance. */
		branchSignatures: (org: string, repo: string, signal?: AbortSignal) =>
			request<RefSignature[]>("GET", `${repoPath(org, repo)}/signatures`, undefined, signal),
		configStatus: (org: string, repo: string, signal?: AbortSignal) =>
			request<ConfigStatus>("GET", `${repoPath(org, repo)}/config/status`, undefined, signal),
		listOrgs: (signal?: AbortSignal) => request<OrgSummary[]>("GET", "/-/api/orgs", undefined, signal),
		createOrg: (slug: string) => request<OrgStatus>("POST", "/-/api/orgs", { slug }, undefined, "admin"),
		getOrg: (org: string, signal?: AbortSignal) =>
			request<OrgStatus>("GET", `/-/api/orgs/${segment(org)}`, undefined, signal),
		listRepos: (org: string, signal?: AbortSignal) =>
			request<RepoSummary[]>("GET", `/-/api/orgs/${segment(org)}/repos`, undefined, signal),
		createRepo: (org: string, slug: string) =>
			request<RepoInfo>("POST", `/-/api/orgs/${segment(org)}/repos`, { slug }, undefined, "admin"),
		getRepo: (org: string, repo: string, signal?: AbortSignal) =>
			request<RepoView>("GET", repoPath(org, repo), undefined, signal),
		listExtensions: (org: string, repo: string, signal?: AbortSignal) =>
			request<Installation[]>("GET", `${repoPath(org, repo)}/extensions`, undefined, signal),
		call,
		issues,
		kanban,
	};
};

export type Client = ReturnType<typeof createClient>;

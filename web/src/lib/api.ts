/**
 * A typed client for the Tartan gateway's HTTP API and for the first-party extensions'
 * tools, which the gateway exposes at
 * `POST /-/api/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}`.
 *
 * Every request carries the admin bearer token. Failures become `ApiError`s that keep the
 * gateway's problem body (`{error, kind?}`) so views can show exactly what the forge said.
 */

import type {
	BoardList,
	BoardView,
	CallKind,
	Card,
	Installation,
	Issue,
	IssueChange,
	IssueCreateInput,
	IssueListInput,
	IssueListOutput,
	IssueUpdateInput,
	OrgStatus,
	OrgSummary,
	ProblemBody,
	PropertyDef,
	RepoInfo,
	RepoSummary,
	RepoView,
} from "./types.ts";

/** The id Issues installs under. */
export const ISSUES_ID = "rawkode.issues";
/** The id Kanban installs under. */
export const KANBAN_ID = "rawkode.kanban";

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
	/** Read on every request, so a changed token applies at once. */
	token: () => string;
	fetch?: typeof globalThis.fetch;
}

const isProblem = (value: unknown): value is ProblemBody =>
	typeof value === "object" && value !== null &&
	typeof (value as { error?: unknown }).error === "string";

const segment = (value: string): string => encodeURIComponent(value);

/** Creates a client for one gateway. */
export const createClient = (options: ClientOptions) => {
	const baseUrl = (options.baseUrl ?? "").replace(/\/+$/, "");
	const doFetch = options.fetch ?? ((...args: Parameters<typeof fetch>) => fetch(...args));

	const request = async <T>(
		method: "GET" | "POST",
		path: string,
		body?: unknown,
		signal?: AbortSignal,
	): Promise<T> => {
		const headers: Record<string, string> = {
			accept: "application/json",
			authorization: `Bearer ${options.token()}`,
		};
		const init: RequestInit = { method, headers };
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
			const problem = isProblem(parsed) ? parsed : null;
			const message = problem?.error ??
				(response.status >= 500 && text.length === 0
					? "The gateway did not answer (is it running?)"
					: text.trim() || response.statusText || `HTTP ${response.status}`);
			throw new ApiError(response.status, problem, message, method, path);
		}
		return parsed as T;
	};

	const repoPath = (org: string, repo: string) => `/-/api/orgs/${segment(org)}/repos/${segment(repo)}`;

	/** Calls an extension installed on a repository. */
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
		);

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
		listOrgs: (signal?: AbortSignal) => request<OrgSummary[]>("GET", "/-/api/orgs", undefined, signal),
		createOrg: (slug: string) => request<OrgStatus>("POST", "/-/api/orgs", { slug }),
		getOrg: (org: string, signal?: AbortSignal) =>
			request<OrgStatus>("GET", `/-/api/orgs/${segment(org)}`, undefined, signal),
		listRepos: (org: string, signal?: AbortSignal) =>
			request<RepoSummary[]>("GET", `/-/api/orgs/${segment(org)}/repos`, undefined, signal),
		createRepo: (org: string, slug: string) =>
			request<RepoInfo>("POST", `/-/api/orgs/${segment(org)}/repos`, { slug }),
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

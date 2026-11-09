import { describe, expect, test, vi } from "vitest";
import { ApiError, createClient } from "./api.ts";
import { createSandboxFetch } from "./sandbox.ts";

/** A fetch stub that records requests and answers with the given response. */
const stub = (status: number, body?: unknown, raw?: string) => {
	const calls: { url: string; init: RequestInit }[] = [];
	const fetch = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
		calls.push({ url: String(url), init: init ?? {} });
		return new Response(raw ?? (body === undefined ? null : JSON.stringify(body)), {
			status,
			headers: { "content-type": "application/json" },
		});
	});
	return { calls, fetch: fetch as unknown as typeof globalThis.fetch };
};

describe("requests", () => {
	test("sends the bearer token and JSON body to the contract's paths", async () => {
		const { calls, fetch } = stub(201, { id: "o1", slug: "acme", pool: null });
		const client = createClient({ token: () => "secret", fetch, baseUrl: "http://gw/" });
		await client.createOrg("acme");
		expect(calls[0]!.url).toBe("http://gw/-/api/orgs");
		expect(calls[0]!.init.method).toBe("POST");
		const headers = calls[0]!.init.headers as Record<string, string>;
		expect(headers.authorization).toBe("Bearer secret");
		expect(headers["content-type"]).toBe("application/json");
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ slug: "acme" });
	});

	test("reads the token on every request", async () => {
		const { calls, fetch } = stub(200, []);
		let token = "one";
		const client = createClient({ token: () => token, fetch });
		await client.listOrgs();
		token = "two";
		await client.listOrgs();
		expect(calls.map((c) => (c.init.headers as Record<string, string>).authorization)).toEqual([
			"Bearer one",
			"Bearer two",
		]);
	});

	test.each([
		["listOrgs", (c: ReturnType<typeof createClient>) => c.listOrgs(), "GET", "/-/api/orgs"],
		["getOrg", (c: ReturnType<typeof createClient>) => c.getOrg("acme"), "GET", "/-/api/orgs/acme"],
		["listRepos", (c: ReturnType<typeof createClient>) => c.listRepos("acme"), "GET", "/-/api/orgs/acme/repos"],
		["createRepo", (c: ReturnType<typeof createClient>) => c.createRepo("acme", "api"), "POST", "/-/api/orgs/acme/repos"],
		["getRepo", (c: ReturnType<typeof createClient>) => c.getRepo("acme", "api"), "GET", "/-/api/orgs/acme/repos/api"],
		[
			"listExtensions",
			(c: ReturnType<typeof createClient>) => c.listExtensions("acme", "api"),
			"GET",
			"/-/api/orgs/acme/repos/api/extensions",
		],
	])("%s", async (_name, call, method, path) => {
		const { calls, fetch } = stub(200, {});
		await call(createClient({ token: () => "t", fetch }));
		expect(calls[0]!.init.method).toBe(method);
		expect(calls[0]!.url).toBe(path);
	});

	test("calls extension tools through the repository call route", async () => {
		const { calls, fetch } = stub(200, { card: { number: 3 } });
		const client = createClient({ token: () => "t", fetch });
		const card = await client.kanban("acme", "api").move("triage", 3, "done");
		expect(card).toEqual({ number: 3 });
		expect(calls[0]!.url).toBe("/-/api/orgs/acme/repos/api/extensions/kanban.tartan.rawkode.academy/tools/move");
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ board: "triage", number: 3, column: "done" });
	});

	test("issues tools use properties, not fields", async () => {
		const { calls, fetch } = stub(200, { issue: { number: 1 } });
		const client = createClient({ token: () => "t", fetch });
		await client.issues("acme", "api").create({ title: "x", properties: { status: "todo" } });
		expect(calls[0]!.url).toBe("/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/create");
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ title: "x", properties: { status: "todo" } });
	});

	test("encodes path segments", async () => {
		const { calls, fetch } = stub(200, {});
		await createClient({ token: () => "t", fetch }).call("a b", "c/d", "x.y", "tools", "n?", {});
		expect(calls[0]!.url).toBe("/-/api/orgs/a%20b/repos/c%2Fd/extensions/x.y/tools/n%3F");
	});
});

describe("errors", () => {
	test("keep the gateway's problem body and extension kind", async () => {
		const { fetch } = stub(400, { error: "status: Must be one of the options.", kind: "invalid" });
		const client = createClient({ token: () => "t", fetch });
		const error = await client.issues("acme", "api").update({ number: 1 }).catch((e: unknown) => e);
		expect(error).toBeInstanceOf(ApiError);
		const e = error as ApiError;
		expect(e.status).toBe(400);
		expect(e.kind).toBe("invalid");
		expect(e.message).toBe("status: Must be one of the options.");
		expect(e.method).toBe("POST");
	});

	test("explain an unreachable gateway", async () => {
		const fetch = vi.fn(async () => {
			throw new TypeError("fetch failed");
		}) as unknown as typeof globalThis.fetch;
		const error = (await createClient({ token: () => "t", fetch }).listOrgs().catch((e: unknown) => e)) as ApiError;
		expect(error.status).toBe(0);
		expect(error.message).toMatch(/Could not reach the gateway/);
	});

	test("explain an empty 5xx from the dev proxy", async () => {
		const { fetch } = stub(502, undefined, "");
		const error = (await createClient({ token: () => "t", fetch }).listOrgs().catch((e: unknown) => e)) as ApiError;
		expect(error.status).toBe(502);
		expect(error.problem).toBeNull();
		expect(error.message).toMatch(/did not answer/);
	});

	test("pass aborts through untouched", async () => {
		const fetch = vi.fn(async () => {
			throw new DOMException("aborted", "AbortError");
		}) as unknown as typeof globalThis.fetch;
		await expect(createClient({ token: () => "t", fetch }).listOrgs()).rejects.toMatchObject({ name: "AbortError" });
	});
});

describe("against the sandbox gateway", () => {
	const client = () => createClient({ token: () => "dev-admin-token", fetch: createSandboxFetch() });

	test("refuses a wrong token with 401", async () => {
		const c = createClient({ token: () => "nope", fetch: createSandboxFetch() });
		await expect(c.listOrgs()).rejects.toMatchObject({ status: 401, message: "unauthorized" });
	});

	test("creates and lists orgs and repositories", async () => {
		const c = client();
		await c.createOrg("globex");
		expect((await c.listOrgs()).map((o) => o.slug)).toEqual(["acme", "globex"]);
		const repo = await c.createRepo("globex", "web");
		expect(repo).toMatchObject({ org_slug: "globex", slug: "web" });
		expect(await c.listRepos("globex")).toEqual([
			{ slug: "web", path: "globex/web", clone_url: repo.clone_url },
		]);
		expect(await c.listExtensions("globex", "web")).toEqual([]);
	});

	test("refuses an invalid slug and an unknown org", async () => {
		const c = client();
		await expect(c.createOrg("Bad_Slug")).rejects.toMatchObject({ status: 400 });
		await expect(c.getOrg("missing")).rejects.toMatchObject({ status: 404, message: "org missing not found" });
	});

	test("creates, updates, closes and reopens an issue", async () => {
		const issues = client().issues("acme", "api");
		const created = await issues.create({ title: "Typed", properties: { priority: "p2", budget: 1999 } });
		expect(created.properties).toMatchObject({ priority: "p2", budget: 1999, status: "todo" });
		const updated = await issues.update({
			number: created.number,
			expectedVersion: created.version,
			properties: { priority: null, tags: ["docs"] },
		});
		expect(updated.changed).toBe(true);
		expect(updated.issue.properties.priority).toBeUndefined();
		expect(updated.issue.properties.tags).toEqual(["docs"]);
		await expect(
			issues.update({ number: created.number, expectedVersion: created.version, title: "stale" }),
		).rejects.toMatchObject({ status: 409, kind: "conflict" });
		expect((await issues.close(created.number)).issue.state).toBe("closed");
		expect((await issues.reopen(created.number)).issue.closedAt).toBeNull();
	});

	test("refuses a value that does not fit its property", async () => {
		const issues = client().issues("acme", "api");
		await expect(issues.create({ title: "x", properties: { link: "not a url" } })).rejects.toMatchObject({
			status: 400,
			kind: "invalid",
		});
	});

	test("lists boards and moves a card through Issues", async () => {
		const c = client();
		const kanban = c.kanban("acme", "api");
		const { boards } = await kanban.listBoards();
		expect(boards.map((b) => b.id)).toEqual(["triage", "gateway", "open-closed"]);
		const view = await kanban.board("triage");
		expect(view.board.columns.map((col) => col.title)).toEqual(["To do", "In progress", "In review", "Done"]);
		expect(view.board.lanes).toEqual({ by: "priority" });
		expect(view.lanes.at(-1)).toMatchObject({ value: null, title: "None" });
		expect(view.unplaced.map((card) => card.number)).toEqual([7]);
		const card = await kanban.move("triage", 2, "done");
		expect(card.properties.status).toBe("done");
		expect((await c.issues("acme", "api").get(2)).properties.status).toBe("done");
	});

	test("a board without lanes or groups has one lane and one group per cell", async () => {
		const view = await client().kanban("acme", "api").board("gateway");
		expect(view.lanes).toHaveLength(1);
		expect(view.lanes[0]).toMatchObject({ value: null, title: "" });
		for (const cell of view.lanes[0]!.cells) {
			expect(cell.groups).toEqual([{ value: null, title: "", cards: expect.any(Array) }]);
		}
	});
});

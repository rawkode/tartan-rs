/** Helpers for component tests: a recorded in-memory gateway and the app's routes in memory. */

import { flushPromises, mount } from "@vue/test-utils";
import { vi } from "vitest";
import { type Component, defineComponent, h } from "vue";
import { createMemoryHistory, createRouter, RouterView } from "vue-router";
import { setAccount } from "./lib/account.ts";
import { createSandboxFetch, sampleState, type SandboxState } from "./lib/sandbox.ts";
import { guard, routes } from "./router.ts";
import type { Me } from "./lib/types.ts";

/** Answers from `state` and records each request as `METHOD /path`. */
export const gateway = (state: SandboxState = sampleState()) => {
	const calls: string[] = [];
	const inner = createSandboxFetch({ state });
	const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
		const url = new URL(String(input), "http://forge.invalid");
		calls.push(`${init?.method ?? "GET"} ${url.pathname}`);
		return inner(input, init);
	});
	vi.stubGlobal("fetch", fetch);
	return { state, calls, fetch };
};

/** A state whose caller is `me`, with the store already knowing it. */
export const as = (me: Me, state: SandboxState = sampleState()) => {
	state.me = me;
	setAccount(me);
	return gateway(state);
};

/** The app's routes and guard in memory history. */
export const appRouter = () => {
	const router = createRouter({ history: createMemoryHistory(), routes });
	guard(router);
	return router;
};

const Shell = defineComponent({ render: () => h(RouterView) });

/** Mounts `root` (the bare routed view by default) at `path`, once its loads have settled. */
export const mountAt = async (path: string, root: Component = Shell) => {
	const router = appRouter();
	await router.push(path);
	await router.isReady();
	const wrapper = mount(root, { global: { plugins: [router] }, attachTo: document.body });
	await settle();
	return { wrapper, router };
};

/** Lets pending requests, their sandbox latency-free answers and re-renders finish. */
export const settle = async () => {
	for (let i = 0; i < 10; i++) {
		await flushPromises();
		await new Promise((r) => setTimeout(r, 0));
	}
};

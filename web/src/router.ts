/**
 * Routes, in the URL hash so the static build works from any file server and deep links
 * survive reloads without server rewrites. Org and repository slugs never start with `-`,
 * so the forge's own pages (`/-/settings`, `/-/sign-in`, `/-/cli-login`) cannot collide with
 * an org.
 *
 * Every page but sign-in needs to know who the caller is: the guard reads `GET /-/api/me`
 * once and sends an anonymous visitor to sign in, with `return_to` naming the page they asked
 * for. There is no public view: anonymous callers get 401 on all of `/-/api`.
 */

import { createRouter, createWebHashHistory, type Router, type RouteRecordRaw } from "vue-router";
import { account } from "./lib/account.ts";
import { loadAccount } from "./lib/client.ts";
import { returnToFor } from "./lib/signin.ts";

export const routes: RouteRecordRaw[] = [
	{ path: "/", name: "orgs", component: () => import("./views/OrgsView.vue") },
	{ path: "/-/settings", name: "settings", component: () => import("./views/SettingsView.vue") },
	{
		path: "/-/sign-in",
		name: "sign-in",
		component: () => import("./views/SignInView.vue"),
		meta: { public: true },
	},
	{ path: "/-/cli-login", name: "cli-login", component: () => import("./views/CliLoginView.vue") },
	{ path: "/:org", name: "org", component: () => import("./views/OrgView.vue"), props: true },
	{
		path: "/:org/:repo",
		component: () => import("./views/RepoLayout.vue"),
		props: (route) => ({ org: route.params.org, repo: route.params.repo }),
		children: [
			{ path: "", name: "repo", component: () => import("./views/RepoOverview.vue") },
			{ path: "issues", name: "issues", component: () => import("./views/IssuesView.vue") },
			{ path: "issues/new", name: "issue-new", component: () => import("./views/IssueNewView.vue") },
			{
				path: "issues/:number(\\d+)",
				name: "issue",
				component: () => import("./views/IssueView.vue"),
			},
			{ path: "boards/:board?", name: "boards", component: () => import("./views/BoardsView.vue") },
			{
				path: "commit/:sha([0-9a-f]{40,64})/signature",
				name: "commit-signature",
				component: () => import("./views/CommitSignatureView.vue"),
			},
		],
	},
	{ path: "/:rest(.*)*", name: "not-found", component: () => import("./views/NotFoundView.vue") },
];

/** Pages a person the forge does not admit may still use: sign-in and approving `tt`. */
export const OPEN_TO_EVERYONE_SIGNED_IN = ["sign-in", "cli-login"];

/** Sends anonymous visitors to sign in, keeping where they were going. */
export const guard = (router: Router): void => {
	router.beforeEach(async (to) => {
		if (to.meta.public) return true;
		await loadAccount();
		if (account.status === "ready" && account.me?.anonymous) {
			return { name: "sign-in", query: { return_to: returnToFor(to.fullPath) } };
		}
		return true;
	});
};

export const router = createRouter({
	history: createWebHashHistory(),
	routes,
	scrollBehavior: (_to, _from, saved) => saved ?? { top: 0 },
});

guard(router);

/**
 * Routes, in the URL hash so the static build works from any file server and deep links
 * survive reloads without server rewrites. Org and repository slugs never start with `-`,
 * so `/-/settings` cannot collide with an org.
 */

import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

const routes: RouteRecordRaw[] = [
	{ path: "/", name: "orgs", component: () => import("./views/OrgsView.vue") },
	{ path: "/-/settings", name: "settings", component: () => import("./views/SettingsView.vue") },
	{ path: "/:org", name: "org", component: () => import("./views/OrgView.vue"), props: true },
	{
		path: "/:org/:repo",
		component: () => import("./views/RepoLayout.vue"),
		props: true,
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
		],
	},
	{ path: "/:rest(.*)*", name: "not-found", component: () => import("./views/NotFoundView.vue") },
];

export const router = createRouter({
	history: createWebHashHistory(),
	routes,
	scrollBehavior: (_to, _from, saved) => saved ?? { top: 0 },
});

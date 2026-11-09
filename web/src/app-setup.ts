/**
 * Configures the Vue app Astro creates for each island: the router, what a 401 does, and the
 * theme.
 */

import type { App } from "vue";
import { watch } from "vue";
import { loadAccount, onUnauthorized } from "./lib/client.ts";
import { resetPeople } from "./lib/people.ts";
import { applyTheme, settings } from "./lib/settings.ts";
import { returnToFor } from "./lib/signin.ts";
import { router } from "./router.ts";

export default (app: App): void => {
	app.use(router);
	// A 401 means the session ended or was never there: sign in, then come back.
	onUnauthorized(() => {
		const current = router.currentRoute.value;
		if (current.name === "sign-in") return;
		void router.push({ name: "sign-in", query: { return_to: returnToFor(current.fullPath) } });
	});
	if (typeof window !== "undefined") {
		watch(() => settings.theme, (theme) => applyTheme(theme), { immediate: true });
		globalThis.matchMedia?.("(prefers-color-scheme: dark)").addEventListener(
			"change",
			() => applyTheme(),
		);
		// The sandbox has its own caller and people.
		watch(() => settings.sandbox, () => {
			resetPeople();
			void loadAccount(true);
		});
	}
};

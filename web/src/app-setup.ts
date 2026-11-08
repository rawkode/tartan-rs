/** Configures the Vue app Astro creates for each island: the router, and the theme. */

import type { App } from "vue";
import { watch } from "vue";
import { applyTheme, settings } from "./lib/settings.ts";
import { router } from "./router.ts";

export default (app: App): void => {
	app.use(router);
	if (typeof window !== "undefined") {
		watch(() => settings.theme, (theme) => applyTheme(theme), { immediate: true });
		globalThis.matchMedia?.("(prefers-color-scheme: dark)").addEventListener(
			"change",
			() => applyTheme(),
		);
	}
};

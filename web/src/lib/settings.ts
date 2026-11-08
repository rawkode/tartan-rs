/**
 * Per-browser settings, kept in `localStorage`: the gateway and Git host tokens, the theme,
 * the address `cue` reaches the gateway at, and whether to use the in-browser sandbox.
 * Storage can be unavailable (private windows, blocked site data), so every access is guarded and
 * the defaults always work.
 */

import { reactive, watch } from "vue";

export type ThemePreference = "system" | "light" | "dark";

export interface Settings {
	/** The gateway's admin bearer token. */
	token: string;
	/** The Git host's token for pushes, shown in the onboarding commands. */
	gitToken: string;
	theme: ThemePreference;
	/**
	 * The gateway's address as `cue` on this machine reaches it. Empty means the
	 * address the dev server forwards to, or this page's origin.
	 */
	gatewayAddress: string;
	/** Serve every request from in-browser sample data instead of a gateway. */
	sandbox: boolean;
	/** How often boards refresh, in seconds. */
	pollSeconds: number;
}

/** The gateway token `scripts/dev.sh` configures. */
export const DEV_TOKEN = "dev-admin-token";
/** The Git host token `scripts/dev.sh` configures for walgit. */
export const DEV_GIT_TOKEN = "dev-git-host-token";

const KEY = "tartan.settings.v1";

export const defaultSettings = (): Settings => ({
	token: DEV_TOKEN,
	gitToken: DEV_GIT_TOKEN,
	theme: "system",
	gatewayAddress: "",
	sandbox: false,
	pollSeconds: 4,
});

const load = (): Settings => {
	const defaults = defaultSettings();
	try {
		const raw = globalThis.localStorage?.getItem(KEY);
		if (!raw) return defaults;
		const stored = JSON.parse(raw) as Partial<Settings>;
		return {
			token: typeof stored.token === "string" ? stored.token : defaults.token,
			gitToken: typeof stored.gitToken === "string" ? stored.gitToken : defaults.gitToken,
			theme: stored.theme === "light" || stored.theme === "dark" || stored.theme === "system"
				? stored.theme
				: defaults.theme,
			gatewayAddress: typeof stored.gatewayAddress === "string"
				? stored.gatewayAddress
				: defaults.gatewayAddress,
			sandbox: stored.sandbox === true,
			pollSeconds: typeof stored.pollSeconds === "number" && stored.pollSeconds >= 1 &&
					stored.pollSeconds <= 60
				? stored.pollSeconds
				: defaults.pollSeconds,
		};
	} catch {
		return defaults;
	}
};

/** The settings, shared by every component. Changes are saved as they happen. */
export const settings = reactive<Settings>(load());

watch(
	settings,
	(value) => {
		try {
			globalThis.localStorage?.setItem(KEY, JSON.stringify(value));
		} catch {
			// Storage is unavailable; the settings last until the page closes.
		}
	},
	{ deep: true },
);

/** The gateway's origin for the command line: the setting, else the dev proxy's target. */
export const gatewayOrigin = (): string => {
	const configured = settings.gatewayAddress.trim().replace(/\/+$/, "");
	if (configured) return configured.includes("://") ? configured : `http://${configured}`;
	const proxied = import.meta.env.PUBLIC_TARTAN_GATEWAY as string | undefined;
	if (proxied) return proxied.replace(/\/+$/, "");
	return globalThis.location?.origin ?? "http://127.0.0.1:8080";
};

/**
 * The `CUE_REGISTRY` value for the gateway: its host and port, marked `+insecure` when it is
 * served over plain HTTP, as `cue` requires.
 */
export const cueRegistry = (origin: string = gatewayOrigin()): string => {
	try {
		const url = new URL(origin);
		return url.protocol === "http:" ? `${url.host}+insecure` : url.host;
	} catch {
		return origin;
	}
};

/** Applies the theme to the document: `data-theme` drives Panda's light and dark tokens. */
export const applyTheme = (preference: ThemePreference = settings.theme): void => {
	if (typeof document === "undefined") return;
	const dark = preference === "dark" ||
		(preference === "system" && globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches);
	document.documentElement.dataset.theme = dark ? "dark" : "light";
};

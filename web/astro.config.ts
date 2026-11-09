import vue from "@astrojs/vue";
import pandacss from "@pandacss/vite";
import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";

/**
 * The gateway the dev server forwards API and registry requests to. The browser only ever
 * talks to its own origin, so the gateway needs no CORS configuration.
 */
const gateway = process.env.TARTAN_GATEWAY ?? "http://127.0.0.1:8080";

// The onboarding panel shows the gateway's address for `CUE_REGISTRY`, which `cue` contacts
// directly rather than through this server.
process.env.PUBLIC_TARTAN_GATEWAY ??= gateway;

// `changeOrigin` rewrites only `Host`, so the browser's `Origin` reaches the gateway unchanged
// for its CSRF check; `xfwd` adds `X-Forwarded-For`, which the gateway trusts from this proxy
// (`TARTAN_TRUSTED_PROXIES`) to rate-limit sign-in per client address.
const forward = { target: gateway, changeOrigin: true, ws: false, xfwd: true } as const;

export default defineConfig({
	output: "static",
	server: { port: 4321, host: "127.0.0.1" },
	integrations: [vue({ appEntrypoint: "/src/app-setup.ts" })],
	vite: {
		plugins: [pandacss()],
		resolve: {
			alias: {
				"styled-system": fileURLToPath(new URL("./styled-system", import.meta.url)),
			},
		},
		server: {
			// The forge's `TARTAN_PUBLIC_URL` names this exact origin, so a server that cannot get
			// its port fails instead of moving to another, where sign-in would not work.
			strictPort: true,
			proxy: {
				"/-/": forward,
				"/v2/": forward,
			},
		},
		preview: {
			proxy: {
				"/-/": forward,
				"/v2/": forward,
			},
		},
	},
});

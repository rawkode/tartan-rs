/// <reference types="vitest/config" />
import { getViteConfig } from "astro/config";

// Astro's Vite configuration (Vue, Panda, the styled-system alias) with a DOM for components.
export default getViteConfig({
	test: {
		environment: "happy-dom",
		include: ["src/**/*.test.ts"],
		setupFiles: ["src/test-setup.ts"],
	},
});

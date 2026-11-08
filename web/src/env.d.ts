/// <reference types="astro/client" />

interface ImportMetaEnv {
	/** The gateway the dev server forwards to, for the command-line snippets. */
	readonly PUBLIC_TARTAN_GATEWAY?: string;
}

interface ImportMeta {
	readonly env: ImportMetaEnv;
}

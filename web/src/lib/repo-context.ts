/** What a repository's pages share: its identity and installed extensions, loaded once. */

import { inject, type InjectionKey, type ComputedRef } from "vue";
import type { Resource } from "./resource.ts";
import type { Installation, RepoView } from "./types.ts";

export interface RepoContext {
	org: ComputedRef<string>;
	repo: ComputedRef<string>;
	info: Resource<RepoView>;
	extensions: Resource<Installation[]>;
	/** The installation of an extension, once the list has loaded. */
	installed: (id: string) => Installation | undefined;
}

export const repoContextKey: InjectionKey<RepoContext> = Symbol("repo");

export const useRepo = (): RepoContext => {
	const context = inject(repoContextKey);
	if (!context) throw new Error("useRepo() outside a repository page");
	return context;
};

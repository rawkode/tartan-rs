/**
 * Loading data into components: a resource holds the latest value, the latest error and
 * whether a load is running, reloads when its sources change, and cancels a superseded or
 * abandoned request.
 */

import { onBeforeUnmount, ref, shallowRef, watch, type Ref, type ShallowRef, type WatchSource } from "vue";

export interface Resource<T> {
	data: ShallowRef<T | undefined>;
	error: ShallowRef<unknown>;
	/** True while a load that shows progress is running. */
	loading: Ref<boolean>;
	/** Loads again; `quiet` keeps the current view instead of showing progress (for polling). */
	reload: (options?: { quiet?: boolean }) => Promise<void>;
	/** Replaces the value locally, as an optimistic update does. */
	set: (value: T) => void;
}

export const useResource = <T>(
	loader: (signal: AbortSignal) => Promise<T>,
	sources: WatchSource[] = [],
): Resource<T> => {
	const data = shallowRef<T>();
	const error = shallowRef<unknown>();
	const loading = ref(false);
	let controller: AbortController | undefined;

	const reload = async ({ quiet = false } = {}): Promise<void> => {
		controller?.abort();
		const current = new AbortController();
		controller = current;
		if (!quiet) loading.value = true;
		try {
			const value = await loader(current.signal);
			if (controller !== current) return;
			data.value = value;
			error.value = undefined;
		} catch (cause) {
			if (controller !== current) return;
			if (cause instanceof DOMException && cause.name === "AbortError") return;
			// A failed background refresh keeps showing the last good data.
			if (!quiet || data.value === undefined) error.value = cause;
		} finally {
			if (controller === current) loading.value = false;
		}
	};

	watch(sources, () => {
		data.value = undefined;
		error.value = undefined;
		void reload();
	}, { immediate: true });
	onBeforeUnmount(() => controller?.abort());

	return { data, error, loading, reload, set: (value: T) => (data.value = value) };
};

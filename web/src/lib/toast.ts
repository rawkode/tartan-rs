/** Short-lived notices: what an action did, or why it failed when no form shows the error. */

import { reactive } from "vue";

export interface Toast {
	id: number;
	tone: "success" | "danger" | "neutral";
	message: string;
}

export const toasts = reactive<Toast[]>([]);

let next = 0;

export const notify = (message: string, tone: Toast["tone"] = "neutral", ms = 4500): void => {
	const id = ++next;
	toasts.push({ id, tone, message });
	setTimeout(() => dismiss(id), ms);
};

export const dismiss = (id: number): void => {
	const index = toasts.findIndex((t) => t.id === id);
	if (index >= 0) toasts.splice(index, 1);
};

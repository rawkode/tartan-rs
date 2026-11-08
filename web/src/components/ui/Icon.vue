<script setup lang="ts">
/** A small stroked icon set drawn on a 24-unit grid. Decorative unless given a label. */

export type IconName =
	| "plus"
	| "x"
	| "check"
	| "chevron-right"
	| "chevron-down"
	| "arrow-left"
	| "refresh"
	| "settings"
	| "copy"
	| "sun"
	| "moon"
	| "monitor"
	| "repo"
	| "org"
	| "issue-open"
	| "issue-closed"
	| "board"
	| "alert"
	| "external"
	| "grip"
	| "search"
	| "puzzle"
	| "eye"
	| "eye-off"
	| "terminal"
	| "move"
	| "inbox"
	| "sandbox";

const props = defineProps<{ name: IconName; size?: number; label?: string }>();

const paths: Record<IconName, string[]> = {
	plus: ["M12 5v14", "M5 12h14"],
	x: ["M6 6l12 12", "M18 6L6 18"],
	check: ["M5 12.5l4.5 4.5L19 7.5"],
	"chevron-right": ["M9.5 6l6 6-6 6"],
	"chevron-down": ["M6 9.5l6 6 6-6"],
	"arrow-left": ["M19 12H5", "M11 6l-6 6 6 6"],
	refresh: ["M20 11a8 8 0 1 0-2.3 5.7", "M20 5v6h-6"],
	settings: [
		"M4 7h10",
		"M18 7h2",
		"M4 17h4",
		"M12 17h8",
		"M16 5v4",
		"M10 15v4",
	],
	copy: ["M9 9h10v10H9z", "M5 15V5h10"],
	sun: [
		"M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z",
		"M12 2v2",
		"M12 20v2",
		"M4.9 4.9l1.4 1.4",
		"M17.7 17.7l1.4 1.4",
		"M2 12h2",
		"M20 12h2",
		"M4.9 19.1l1.4-1.4",
		"M17.7 6.3l1.4-1.4",
	],
	moon: ["M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z"],
	monitor: ["M3 5h18v11H3z", "M8 20h8", "M12 16v4"],
	repo: ["M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5z", "M5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3"],
	org: ["M4 21V7l8-4 8 4v14", "M9 21v-5h6v5", "M9 10h.01", "M15 10h.01"],
	"issue-open": ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M12 11a1 1 0 1 0 0 2 1 1 0 0 0 0-2z"],
	"issue-closed": ["M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z", "M8 12.5l2.5 2.5L16 9.5"],
	board: ["M4 4h16v16H4z", "M9.5 4v16", "M14.5 4v16"],
	alert: ["M12 3l9.5 17h-19z", "M12 10v4", "M12 17.5h.01"],
	external: ["M14 4h6v6", "M20 4l-9 9", "M18 14v6H4V6h6"],
	grip: ["M9 6h.01", "M15 6h.01", "M9 12h.01", "M15 12h.01", "M9 18h.01", "M15 18h.01"],
	search: ["M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z", "M20 20l-4-4"],
	puzzle: [
		"M9 4h3a2 2 0 1 1 4 0h3v5a2 2 0 1 1 0 4v6h-6a2 2 0 1 0-4 0H4v-6a2 2 0 1 0 0-4V4z",
	],
	eye: ["M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z", "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z"],
	"eye-off": ["M3 3l18 18", "M10.6 5.1A10 10 0 0 1 12 5c6.5 0 10 7 10 7a17 17 0 0 1-3 3.9", "M6.6 6.6C3.8 8.4 2 12 2 12s3.5 7 10 7a9.6 9.6 0 0 0 5.4-1.6", "M9.9 9.9a3 3 0 0 0 4.2 4.2"],
	terminal: ["M4 5h16v14H4z", "M8 10l2.5 2L8 14", "M13 14h3"],
	move: ["M5 12h14", "M15 8l4 4-4 4"],
	inbox: ["M3 13l3-8h12l3 8v6H3z", "M3 13h5l1.5 2.5h5L16 13h5"],
	sandbox: ["M4 20h16", "M6 20l2-8h8l2 8", "M12 12V4", "M12 4l5 3-5 2"],
};

const filled = (name: IconName) => name === "grip";
</script>

<template>
	<svg
		:width="props.size ?? 16"
		:height="props.size ?? 16"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		:stroke-width="filled(props.name) ? 3 : 1.9"
		stroke-linecap="round"
		stroke-linejoin="round"
		:role="props.label ? 'img' : undefined"
		:aria-label="props.label"
		:aria-hidden="props.label ? undefined : 'true'"
		focusable="false"
		style="flex-shrink: 0"
	>
		<path v-for="(d, i) in paths[props.name]" :key="i" :d="d" />
	</svg>
</template>

<script setup lang="ts">
/** A compact line of an issue's set properties, for lists and cards. Long text is left out. */
import { computed } from "vue";
import { css } from "styled-system/css";
import { formatValue, isEmpty, optionTitle, propertyTitle } from "../../lib/properties.ts";
import type { PropertyDef, PropertyValue } from "../../lib/types.ts";
import PersonDisplay from "../PersonDisplay.vue";

const props = defineProps<{
	defs: PropertyDef[];
	values: Record<string, PropertyValue>;
	/** Properties not to show, such as the board's columns. */
	exclude?: string[];
	limit?: number;
}>();

interface Chip {
	key: string;
	text: string;
	title: string;
	tone: "option" | "plain" | "flag";
	/** A person's DID, shown by handle after the text. */
	person?: string;
}

const chips = computed<Chip[]>(() => {
	const out: Chip[] = [];
	for (const def of props.defs) {
		if (props.exclude?.includes(def.name) || def.kind === "longtext") continue;
		const value = props.values[def.name];
		if (isEmpty(value)) continue;
		const label = propertyTitle(def);
		if (def.kind === "multiselect" && Array.isArray(value)) {
			for (const option of value) {
				out.push({ key: `${def.name}:${option}`, text: optionTitle(def, option), title: label, tone: "option" });
			}
		} else if (def.kind === "person" && typeof value === "string") {
			out.push({ key: def.name, text: `${label}:`, title: label, tone: "plain", person: value });
		} else if (def.kind === "boolean") {
			if (value === true) out.push({ key: def.name, text: label, title: label, tone: "flag" });
		} else {
			out.push({
				key: def.name,
				text: def.kind === "select" ? formatValue(def, value) : `${label}: ${formatValue(def, value)}`,
				title: label,
				tone: def.kind === "select" ? "option" : "plain",
			});
		}
	}
	return props.limit ? out.slice(0, props.limit) : out;
});

const styles = {
	row: css({ display: "flex", flexWrap: "wrap", gap: "1" }),
	chip: css({
		display: "inline-flex",
		alignItems: "center",
		maxW: "full",
		h: "5",
		px: "1.5",
		borderRadius: "sm",
		fontSize: "xs",
		fontWeight: "medium",
		borderWidth: "1px",
		whiteSpace: "nowrap",
		overflow: "hidden",
		textOverflow: "ellipsis",
	}),
	option: css({ bg: "accent.subtle", color: "accent.fg", borderColor: "accent.border" }),
	plain: css({ bg: "bg.subtle", color: "fg.muted", borderColor: "border.default" }),
	flag: css({ bg: "warning.subtle", color: "warning.fg", borderColor: "warning.border" }),
};
</script>

<template>
	<div v-if="chips.length" :class="styles.row">
		<span v-for="chip in chips" :key="chip.key" :class="[styles.chip, styles[chip.tone]]" :title="chip.title">
			{{ chip.text }}<template v-if="chip.person">&nbsp;<PersonDisplay :did="chip.person" /></template>
		</span>
	</div>
</template>

<script setup lang="ts">
/**
 * A card on a board. It can be dragged to another column, or moved from the keyboard with
 * its column picker.
 */
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { css } from "styled-system/css";
import type { BoardColumn, Card, PropertyDef } from "../../lib/types.ts";
import PropertyChips from "../issues/PropertyChips.vue";
import Icon from "../ui/Icon.vue";

const props = defineProps<{
	card: Card;
	columns: BoardColumn[];
	/** The column it is in, or `null` in the unplaced tray. */
	column: string | null;
	defs: PropertyDef[];
	/** Properties the board already shows by position (lanes, groups). */
	exclude: string[];
	showProject: boolean;
	/** Where the card's issue opens, when Issues is installed. */
	issueLink?: { name: string; params: Record<string, string | number> };
	pending: boolean;
	dragging: boolean;
}>();
const emit = defineEmits<{
	move: [column: string];
	dragstart: [event: DragEvent];
	dragend: [];
}>();

const targets = computed(() => props.columns.filter((c) => c.value !== props.column));

const styles = {
	card: css({
		position: "relative",
		display: "grid",
		gap: "2",
		p: "3",
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "md",
		boxShadow: "xs",
		cursor: "grab",
		transition: "box-shadow 0.15s, border-color 0.15s, opacity 0.15s, transform 0.15s",
		_hover: { borderColor: "border.strong", boxShadow: "sm" },
		_focusWithin: { borderColor: "accent.solid" },
		"&:active": { cursor: "grabbing" },
	}),
	dragging: css({ opacity: 0.4, transform: "rotate(1deg)" }),
	pending: css({ borderStyle: "dashed", borderColor: "accent.border" }),
	top: css({ display: "flex", alignItems: "center", gap: "1.5", fontSize: "2xs", color: "fg.subtle" }),
	closed: css({ color: "closed", display: "inline-flex", alignItems: "center", gap: "1" }),
	number: css({ fontFamily: "mono" }),
	project: css({ ml: "auto", fontFamily: "mono", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxW: "50%" }),
	title: css({ fontWeight: "medium", fontSize: "sm", lineHeight: "snug", color: "fg.default", wordBreak: "break-word" }),
	link: css({
		_hover: { color: "accent.fg", textDecoration: "underline" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px", borderRadius: "sm" },
	}),
	foot: css({ display: "flex", alignItems: "center", gap: "1" }),
	grip: css({ color: "fg.subtle" }),
	move: css({
		ml: "auto",
		h: "6",
		maxW: "36",
		px: "1.5",
		fontSize: "2xs",
		color: "fg.muted",
		bg: "bg.subtle",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "sm",
		cursor: "pointer",
		opacity: { base: 1, md: 0 },
		_groupHover: { opacity: 1 },
		_groupFocusWithin: { opacity: 1 },
		_focusVisible: { opacity: 1, outline: "2px solid", outlineColor: "focus" },
	}),
};
</script>

<template>
	<article
		:class="['group', styles.card, props.dragging && styles.dragging, props.pending && styles.pending]"
		draggable="true"
		:aria-label="`#${props.card.number} ${props.card.title}`"
		:aria-busy="props.pending"
		@dragstart="emit('dragstart', $event)"
		@dragend="emit('dragend')"
	>
		<div :class="styles.top">
			<Icon name="grip" :size="12" :class="styles.grip" />
			<span :class="styles.number">#{{ props.card.number }}</span>
			<span v-if="props.card.state === 'closed'" :class="styles.closed"><Icon name="issue-closed" :size="12" /> closed</span>
			<span v-if="props.showProject && props.card.project" :class="styles.project" :title="props.card.project">{{ props.card.project }}</span>
		</div>
		<div :class="styles.title">
			<RouterLink v-if="props.issueLink" :to="props.issueLink" :class="styles.link" draggable="false">{{ props.card.title }}</RouterLink>
			<span v-else>{{ props.card.title }}</span>
		</div>
		<PropertyChips :defs="props.defs" :values="props.card.properties" :exclude="props.exclude" :limit="4" />
		<div :class="styles.foot">
			<select
				:class="styles.move"
				:aria-label="`Move #${props.card.number} to another column`"
				value=""
				:disabled="props.pending"
				@change="emit('move', ($event.target as HTMLSelectElement).value); ($event.target as HTMLSelectElement).value = ''"
			>
				<option value="" disabled>Move to…</option>
				<option v-for="c in targets" :key="c.value" :value="c.value">{{ c.title }}</option>
			</select>
		</div>
	</article>
</template>

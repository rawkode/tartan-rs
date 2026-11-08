<script setup lang="ts">
/**
 * Edits one issue property with the control its kind calls for. Emits the value Issues
 * stores (numbers, `YYYY-MM-DD` dates, minor units for money, option lists for
 * multi-selects), or `null` when cleared.
 */
import { computed, ref, useId, watch } from "vue";
import { css } from "styled-system/css";
import { button, field } from "styled-system/recipes";
import {
	fromMinorUnits,
	isHttpUrl,
	minorDigits,
	optionTitle,
	propertyTitle,
	toMinorUnits,
} from "../../lib/properties.ts";
import type { PropertyDef, PropertyValue } from "../../lib/types.ts";
import Icon from "../ui/Icon.vue";
import MarkdownEditor from "./MarkdownEditor.vue";

const props = defineProps<{
	def: PropertyDef;
	modelValue: PropertyValue | undefined;
	error?: string | null;
	/** People the person field suggests. */
	people?: string[];
}>();
const emit = defineEmits<{ "update:modelValue": [value: PropertyValue] }>();

const id = useId();
const hintId = `${id}-hint`;
const set = (value: PropertyValue) => emit("update:modelValue", value);

const text = computed(() =>
	typeof props.modelValue === "string" ? props.modelValue : props.modelValue == null ? "" : String(props.modelValue)
);
const setText = (value: string) => set(value === "" ? null : value);

const setNumber = (raw: string) => {
	if (raw.trim() === "") return set(null);
	const n = Number(raw);
	set(Number.isFinite(n) ? n : raw);
};

const options = computed(() => props.def.options ?? []);
const selected = computed(() => (Array.isArray(props.modelValue) ? props.modelValue : []));
const toggleOption = (option: string) => {
	const has = selected.value.includes(option);
	const next = has
		? selected.value.filter((o) => o !== option)
		: options.value.filter((o) => o === option || selected.value.includes(o));
	set(next.length === 0 ? null : next);
};

const currency = computed(() => props.def.currency ?? "USD");
const moneyText = ref("");
watch(
	() => props.modelValue,
	(value) => {
		if (props.def.kind !== "money") return;
		if (typeof value === "number") {
			if (toMinorUnits(moneyText.value, currency.value) !== value) {
				moneyText.value = fromMinorUnits(value, currency.value);
			}
		} else if (value == null) {
			moneyText.value = "";
		}
	},
	{ immediate: true },
);
const setMoney = (raw: string) => {
	moneyText.value = raw;
	if (raw.trim() === "") return set(null);
	const minor = toMinorUnits(raw, currency.value);
	// An amount that does not parse is passed on as text, so validation can explain it.
	set(minor ?? raw);
};
const moneyStep = computed(() => (minorDigits(currency.value) === 0 ? "1" : `0.${"0".repeat(minorDigits(currency.value) - 1)}1`));

const hint = computed(() => {
	const d = props.def;
	switch (d.kind) {
		case "number":
			if (d.min !== undefined && d.max !== undefined) return `${d.min} to ${d.max}`;
			if (d.min !== undefined) return `At least ${d.min}`;
			if (d.max !== undefined) return `At most ${d.max}`;
			return undefined;
		case "money":
			return `In ${currency.value}; stored in minor units`;
		case "person":
			return "A principal id, such as admin";
		case "shorttext":
			return d.maxLength ? `Up to ${d.maxLength} characters` : undefined;
		default:
			return undefined;
	}
});

const describedBy = computed(() => (props.error || hint.value ? hintId : undefined));

const styles = {
	root: css({ display: "grid", gap: "1.5" }),
	label: css({ display: "flex", alignItems: "baseline", gap: "2", fontSize: "xs", fontWeight: "semibold", color: "fg.muted" }),
	kind: css({ fontWeight: "normal", color: "fg.subtle", fontFamily: "mono", fontSize: "2xs" }),
	hint: css({ fontSize: "2xs", color: "fg.subtle" }),
	error: css({ fontSize: "xs", color: "danger.fg" }),
	affix: css({ display: "flex", alignItems: "stretch" }),
	prefix: css({
		display: "grid",
		placeItems: "center",
		px: "2.5",
		fontSize: "xs",
		fontFamily: "mono",
		color: "fg.muted",
		bg: "bg.subtle",
		borderWidth: "1px",
		borderRightWidth: 0,
		borderColor: "border.default",
		borderLeftRadius: "md",
	}),
	joined: css({ borderLeftRadius: "0 !important" }),
	chips: css({ display: "flex", flexWrap: "wrap", gap: "1.5" }),
	chip: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "1",
		h: "7",
		px: "2.5",
		fontSize: "xs",
		borderRadius: "full",
		borderWidth: "1px",
		borderColor: "border.default",
		bg: "bg.surface",
		color: "fg.muted",
		cursor: "pointer",
		_hover: { borderColor: "border.strong", color: "fg.default" },
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "1px" },
		"&[aria-pressed=true]": { bg: "accent.subtle", borderColor: "accent.border", color: "accent.fg" },
	}),
	switchRow: css({ display: "flex", alignItems: "center", gap: "2", cursor: "pointer", w: "fit-content" }),
	switch: css({
		position: "relative",
		w: "9",
		h: "5",
		borderRadius: "full",
		bg: "bg.muted",
		transition: "background 0.15s",
		flexShrink: 0,
		cursor: "pointer",
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
		"&[aria-checked=true]": { bg: "accent.solid" },
		"&::after": {
			content: '""',
			position: "absolute",
			top: "2px",
			left: "2px",
			w: "4",
			h: "4",
			borderRadius: "full",
			bg: "white",
			boxShadow: "sm",
			transition: "transform 0.15s",
		},
		"&[aria-checked=true]::after": { transform: "translateX(16px)" },
	}),
	switchText: css({ fontSize: "sm", color: "fg.muted" }),
	inline: css({ display: "flex", gap: "1.5", alignItems: "center" }),
};
</script>

<template>
	<div :class="styles.root">
		<label v-if="props.def.kind !== 'boolean' && props.def.kind !== 'multiselect'" :for="id" :class="styles.label">
			{{ propertyTitle(props.def) }} <span :class="styles.kind">{{ props.def.kind }}</span>
		</label>
		<span v-else :id="`${id}-label`" :class="styles.label">
			{{ propertyTitle(props.def) }} <span :class="styles.kind">{{ props.def.kind }}</span>
		</span>

		<input
			v-if="props.def.kind === 'shorttext'"
			:id="id"
			:class="field()"
			type="text"
			:value="text"
			:maxlength="props.def.maxLength ?? 200"
			:aria-invalid="!!props.error"
			:aria-describedby="describedBy"
			@input="setText(($event.target as HTMLInputElement).value)"
		/>

		<MarkdownEditor
			v-else-if="props.def.kind === 'longtext'"
			:id="id"
			:model-value="text"
			:max-length="props.def.maxLength ?? 20000"
			:invalid="!!props.error"
			:describedby="describedBy"
			:rows="4"
			@update:model-value="setText"
		/>

		<input
			v-else-if="props.def.kind === 'number'"
			:id="id"
			:class="field()"
			type="number"
			step="any"
			:min="props.def.min"
			:max="props.def.max"
			:value="typeof props.modelValue === 'number' ? props.modelValue : text"
			:aria-invalid="!!props.error"
			:aria-describedby="describedBy"
			@input="setNumber(($event.target as HTMLInputElement).value)"
		/>

		<input
			v-else-if="props.def.kind === 'date'"
			:id="id"
			:class="field()"
			type="date"
			:value="text"
			:aria-invalid="!!props.error"
			:aria-describedby="describedBy"
			@input="setText(($event.target as HTMLInputElement).value)"
		/>

		<label v-else-if="props.def.kind === 'boolean'" :class="styles.switchRow">
			<button
				:id="id"
				type="button"
				role="switch"
				:class="styles.switch"
				:aria-checked="props.modelValue === true"
				:aria-labelledby="`${id}-label`"
				@click="set(props.modelValue !== true)"
			/>
			<span :class="styles.switchText">{{ props.modelValue === true ? "Yes" : props.modelValue === false ? "No" : "Not set" }}</span>
		</label>

		<select
			v-else-if="props.def.kind === 'select'"
			:id="id"
			:class="field()"
			:value="text"
			:aria-invalid="!!props.error"
			:aria-describedby="describedBy"
			@change="setText(($event.target as HTMLSelectElement).value)"
		>
			<option value="">—</option>
			<option v-for="option in options" :key="option" :value="option">{{ optionTitle(props.def, option) }}</option>
		</select>

		<div
			v-else-if="props.def.kind === 'multiselect'"
			:class="styles.chips"
			role="group"
			:aria-labelledby="`${id}-label`"
			:aria-describedby="describedBy"
		>
			<button
				v-for="option in options"
				:key="option"
				type="button"
				:class="styles.chip"
				:aria-pressed="selected.includes(option)"
				@click="toggleOption(option)"
			>
				<Icon v-if="selected.includes(option)" name="check" :size="12" />
				{{ optionTitle(props.def, option) }}
			</button>
		</div>

		<template v-else-if="props.def.kind === 'person'">
			<input
				:id="id"
				:class="field({ mono: true })"
				type="text"
				:value="text"
				:list="`${id}-people`"
				placeholder="admin"
				autocomplete="off"
				spellcheck="false"
				:aria-invalid="!!props.error"
				:aria-describedby="describedBy"
				@input="setText(($event.target as HTMLInputElement).value.trim())"
			/>
			<datalist :id="`${id}-people`">
				<option v-for="p in props.people ?? []" :key="p" :value="p" />
			</datalist>
		</template>

		<div v-else-if="props.def.kind === 'url'" :class="styles.inline">
			<input
				:id="id"
				:class="field()"
				type="url"
				:value="text"
				placeholder="https://"
				:aria-invalid="!!props.error"
				:aria-describedby="describedBy"
				@input="setText(($event.target as HTMLInputElement).value.trim())"
			/>
			<a
				v-if="text && isHttpUrl(text)"
				:href="text"
				target="_blank"
				rel="noopener noreferrer"
				:class="button({ size: 'sm', variant: 'ghost' })"
				aria-label="Open link in a new tab"
			><Icon name="external" :size="14" /></a>
		</div>

		<div v-else-if="props.def.kind === 'money'" :class="styles.affix">
			<span :class="styles.prefix" aria-hidden="true">{{ currency }}</span>
			<input
				:id="id"
				:class="[field(), styles.joined]"
				type="text"
				inputmode="decimal"
				:step="moneyStep"
				:value="moneyText"
				:placeholder="fromMinorUnits(0, currency)"
				:aria-invalid="!!props.error"
				:aria-describedby="describedBy"
				@input="setMoney(($event.target as HTMLInputElement).value)"
			/>
		</div>

		<p v-if="props.error" :id="hintId" :class="styles.error">{{ props.error }}</p>
		<p v-else-if="hint" :id="hintId" :class="styles.hint">{{ hint }}</p>
	</div>
</template>

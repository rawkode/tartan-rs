<script setup lang="ts">
/**
 * Chooses a person: type a handle (`alice.bsky.social`, `@alice.bsky.social`) or a DID. A
 * handle is resolved to its DID by the forge, which verifies it both ways; the value is always
 * the DID, shown as a chip with the handle. Suggestions are offered as one-click chips.
 */
import { computed, ref, useId } from "vue";
import { css } from "styled-system/css";
import { button, field } from "styled-system/recipes";
import { ApiError } from "../lib/api.ts";
import { client } from "../lib/client.ts";
import { isDid } from "../lib/did.ts";
import Icon from "./ui/Icon.vue";
import PersonDisplay from "./PersonDisplay.vue";

const props = defineProps<{
	modelValue: string | null | undefined;
	/** DIDs offered with one click, such as the viewer and the issue's author. */
	suggestions?: string[];
	inputId?: string;
	invalid?: boolean;
	describedBy?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: string | null] }>();

const generated = useId();
const id = computed(() => props.inputId ?? generated);
const text = ref("");
const busy = ref(false);
const problem = ref<string | null>(null);

const offered = computed(() =>
	[...new Set(props.suggestions ?? [])].filter((did) => isDid(did) && did !== props.modelValue)
);

const choose = (did: string | null) => {
	problem.value = null;
	text.value = "";
	emit("update:modelValue", did);
};

/** Resolves what was typed: a DID as it is, a handle through the forge. */
const resolve = async () => {
	const typed = text.value.trim().replace(/^@/, "").replace(/^at:\/\//, "");
	if (!typed || busy.value) return;
	if (typed.startsWith("did:")) {
		if (isDid(typed)) choose(typed);
		else problem.value = "That is not a DID Tartan accepts.";
		return;
	}
	busy.value = true;
	problem.value = null;
	try {
		const found = await client.resolveHandle(typed.toLowerCase());
		choose(found.did);
	} catch (cause) {
		problem.value = cause instanceof ApiError && cause.status === 404
			? `No account has the handle ${typed}.`
			: cause instanceof Error
			? cause.message
			: String(cause);
	} finally {
		busy.value = false;
	}
};

const styles = {
	root: css({ display: "grid", gap: "1.5" }),
	chosen: css({
		display: "inline-flex",
		alignItems: "center",
		gap: "1",
		w: "fit-content",
		maxW: "full",
		h: "7",
		pl: "2.5",
		pr: "1",
		borderRadius: "full",
		borderWidth: "1px",
		borderColor: "border.default",
		bg: "bg.subtle",
		fontSize: "sm",
	}),
	row: css({ display: "flex", gap: "2" }),
	chips: css({ display: "flex", gap: "1.5", flexWrap: "wrap", fontSize: "xs", alignItems: "center", color: "fg.muted" }),
	problem: css({ fontSize: "xs", color: "danger.fg" }),
};
</script>

<template>
	<div :class="styles.root">
		<span v-if="props.modelValue" :class="styles.chosen">
			<PersonDisplay :did="props.modelValue" />
			<button type="button" :class="button({ variant: 'ghost', size: 'xs' })" aria-label="Clear" @click="choose(null)">
				<Icon name="x" :size="12" />
			</button>
		</span>
		<div :class="styles.row">
			<input
				:id="id"
				v-model="text"
				:class="field()"
				type="text"
				placeholder="handle or DID"
				autocomplete="off"
				spellcheck="false"
				:aria-invalid="props.invalid || !!problem"
				:aria-describedby="props.describedBy"
				@keydown.enter.prevent="resolve"
				@blur="resolve"
			/>
		</div>
		<div v-if="offered.length" :class="styles.chips">
			<span>Suggested:</span>
			<button
				v-for="did in offered"
				:key="did"
				type="button"
				:class="button({ size: 'xs' })"
				@click="choose(did)"
			>
				<PersonDisplay :did="did" />
			</button>
		</div>
		<p v-if="busy" :class="styles.chips" role="status">Looking up {{ text.trim() }}…</p>
		<p v-if="problem" :class="styles.problem" role="alert">{{ problem }}</p>
	</div>
</template>

<script setup lang="ts">
/** A signature result as a badge, worded and toned by the label table (design D15). */
import { computed, watchEffect } from "vue";
import { css } from "styled-system/css";
import { badge } from "styled-system/recipes";
import { personLabel, requestPeople } from "../lib/people.ts";
import { signatureLabel } from "../lib/signatures.ts";
import type { CommitSignature } from "../lib/types.ts";
import Icon from "./ui/Icon.vue";

const props = defineProps<{ result: CommitSignature }>();

watchEffect(() => {
	if (props.result.did) requestPeople([props.result.did]);
});

const label = computed(() => signatureLabel(props.result, personLabel));
const tone = computed(() => {
	switch (label.value.tone) {
		case "success":
			return "success" as const;
		case "warning":
			return "warning" as const;
		case "danger":
			return "danger" as const;
		default:
			return "neutral" as const;
	}
});

const styles = {
	badge: css({ whiteSpace: "normal", h: "auto", minH: "5", py: "0.5", textAlign: "left" }),
	note: css({ fontSize: "xs", color: "fg.muted", ml: "1" }),
};
</script>

<template>
	<span>
		<span :class="[badge({ tone }), styles.badge]" :data-tone="label.tone" data-signature-badge>
			<Icon name="signature" :size="12" /> {{ label.label }}
		</span>
		<span v-if="label.note" :class="styles.note">{{ label.note }}</span>
	</span>
</template>

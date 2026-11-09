<script setup lang="ts">
/**
 * A person, by DID: `@handle` once the forge knows it, a shortened DID until then, with the
 * DID in a tooltip and a link to the Bluesky profile (none on a development forge, whose
 * people exist only on its own network). Renders a `tartan-ui@1` `person` node too. A value
 * that is not a DID, such as an assignee stored before persons were DIDs, shows as text.
 */
import { computed, watchEffect } from "vue";
import { css } from "styled-system/css";
import { devIdentity } from "../lib/account.ts";
import { isDid } from "../lib/did.ts";
import { personLabel, requestPeople } from "../lib/people.ts";

const props = defineProps<{
	/** The DID, or a legacy id. */
	did?: string | null;
	/** A `tartan-ui@1` node: `{t: "person", did}`. */
	node?: { t: "person"; did: string };
	/** Marks an agent acting for the person: `@handle (agent)`. */
	agent?: boolean;
}>();

const value = computed(() => props.node?.did ?? props.did ?? "");
const valid = computed(() => isDid(value.value));

watchEffect(() => {
	if (valid.value) requestPeople([value.value]);
});

const label = computed(() => (valid.value ? personLabel(value.value) : value.value));
const profile = computed(() =>
	valid.value && !devIdentity.value ? `https://bsky.app/profile/${encodeURIComponent(value.value)}` : null
);

const styles = {
	person: css({ fontWeight: "medium", color: "fg.default", whiteSpace: "nowrap" }),
	link: css({ _hover: { textDecoration: "underline", color: "accent.fg" } }),
	legacy: css({ fontFamily: "mono", fontSize: "0.92em", color: "fg.muted" }),
	agent: css({ color: "fg.muted", fontWeight: "normal" }),
};
</script>

<template>
	<span v-if="!valid" :class="styles.legacy" title="Recorded before people were DIDs">{{ label }}</span>
	<span v-else :class="styles.person" :title="value" data-person>
		<a v-if="profile" :href="profile" :class="styles.link" target="_blank" rel="noopener noreferrer">{{ label }}</a>
		<template v-else>{{ label }}</template>
		<span v-if="props.agent" :class="styles.agent"> (agent)</span>
	</span>
</template>

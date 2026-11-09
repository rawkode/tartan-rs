<script setup lang="ts">
/**
 * A repository's branches and their signatures (design D15, *Repository overview*): a badge
 * per branch head, a banner per branch the signing rule covers that does not comply, and a
 * banner while a configuration pushed to `main` waits for its signature check. In a violation,
 * commits whose signer the rule does not allow are listed first and the rest are counted.
 */
import { computed, watchEffect } from "vue";
import { RouterLink } from "vue-router";
import { css } from "styled-system/css";
import { card } from "styled-system/recipes";
import { ApiError } from "../lib/api.ts";
import { client } from "../lib/client.ts";
import { personLabel, requestPeople } from "../lib/people.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import type { RefSignature, SigningViolation } from "../lib/types.ts";
import Icon from "./ui/Icon.vue";
import ProblemAlert from "./ui/ProblemAlert.vue";
import SignatureBadge from "./SignatureBadge.vue";

const props = defineProps<{ org: string; repo: string }>();

const sources = [() => props.org, () => props.repo, () => settings.sandbox];
const branches = useResource((signal) => client.branchSignatures(props.org, props.repo, signal), sources);
const config = useResource((signal) => client.configStatus(props.org, props.repo, signal), sources);

watchEffect(() => {
	const dids = (branches.data.value ?? []).flatMap((b) => [b.did, ...b.violations.map((v) => v.did)]);
	requestPeople(dids.filter((d): d is string => !!d));
});

const branchName = (ref: string) => ref.replace(/^refs\/heads\//, "");
const failing = computed(() => (branches.data.value ?? []).filter((b) => b.covered && b.compliant === false));

/** How many violations a banner lists when no signer is disallowed. */
const SHOWN = 5;
/** The commits whose signer the rule does not allow; the rest are counted, not listed. */
const listed = (branch: RefSignature): { shown: SigningViolation[]; more: number } => {
	const disallowed = branch.violations.filter((v) => !v.allowed);
	const shown = disallowed.length > 0 ? disallowed : branch.violations.slice(0, SHOWN);
	return { shown, more: branch.violations.length - shown.length };
};

const waiting = computed(() => config.data.value?.pending?.waiting_on === "signature_check" ? config.data.value.pending : null);
/** A forge without the signature routes answers 404: then there is nothing to show. */
const missing = (error: unknown) => error instanceof ApiError && error.status === 404;

const cardStyles = card();
const styles = {
	list: css({ display: "grid", gap: "2" }),
	item: css({ display: "flex", alignItems: "center", gap: "3", flexWrap: "wrap", fontSize: "sm" }),
	ref: css({ fontFamily: "mono", fontWeight: "medium" }),
	link: css({ fontSize: "xs", color: "fg.muted", _hover: { color: "accent.fg" } }),
	banner: css({
		display: "grid",
		gap: "1.5",
		borderWidth: "1px",
		borderRadius: "lg",
		p: "3",
		fontSize: "sm",
		borderColor: "danger.border",
		bg: "danger.subtle",
	}),
	info: css({ borderColor: "border.default", bg: "bg.subtle" }),
	bannerHead: css({ display: "flex", alignItems: "center", gap: "2", fontWeight: "semibold" }),
	violations: css({ display: "grid", gap: "1", listStyle: "disc", pl: "5" }),
	mono: css({ fontFamily: "mono", fontSize: "xs" }),
};
</script>

<template>
	<div v-if="waiting" :class="[styles.banner, styles.info]" role="status" data-config-waiting>
		<div :class="styles.bannerHead"><Icon name="signature" /> A configuration waits for its signature check</div>
		<div>
			The configuration pushed to <code>main</code> in <code :class="styles.mono">{{ waiting.commit.slice(0, 12) }}</code>
			is applied once its signatures are checked.
		</div>
	</div>
	<div v-for="branch in failing" :key="branch.ref" :class="styles.banner" role="alert" data-violation>
		<div :class="styles.bannerHead">
			<Icon name="alert" /> <code>{{ branchName(branch.ref) }}</code> does not comply with the signing rule
		</div>
		<ul :class="styles.violations">
			<li v-for="v in listed(branch).shown" :key="v.oid">
				<code :class="styles.mono">{{ v.oid.slice(0, 12) }}</code>: {{ v.problem }}
				<template v-if="v.did"> ({{ personLabel(v.did) }})</template>
			</li>
		</ul>
		<div v-if="listed(branch).more > 0" data-violation-more>and {{ listed(branch).more }} more</div>
	</div>
	<section
		v-if="!(branches.error.value && missing(branches.error.value)) && (branches.error.value || (branches.data.value?.length ?? 0) > 0)"
		:class="cardStyles.root"
		aria-labelledby="branches-title"
	>
		<div :class="cardStyles.header"><h2 id="branches-title" :class="cardStyles.title">Branches</h2></div>
		<div :class="cardStyles.body">
			<ProblemAlert v-if="branches.error.value" :error="branches.error.value" compact @retry="branches.reload()" />
			<ul v-else :class="styles.list" role="list" data-branches>
				<li v-for="branch in branches.data.value" :key="branch.ref" :class="styles.item">
					<span :class="styles.ref">{{ branchName(branch.ref) }}</span>
					<SignatureBadge :result="branch" />
					<RouterLink
						:to="{ name: 'commit-signature', params: { org: props.org, repo: props.repo, sha: branch.oid } }"
						:class="styles.link"
					>{{ branch.oid.slice(0, 12) }}</RouterLink>
				</li>
			</ul>
		</div>
	</section>
</template>

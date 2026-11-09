<script setup lang="ts">
/**
 * One commit's signature (design D15): its status, the verified signer shown apart from the
 * claimed author and committer, the signature record (its `at://` URI and the repository
 * revision that proved it), the proof bundle and the result's annotations. Owners, and the
 * signer, may ask for an `unverifiable` result to be checked again.
 */
import { computed, ref, watchEffect } from "vue";
import { useRoute } from "vue-router";
import { css } from "styled-system/css";
import { button, card } from "styled-system/recipes";
import PersonDisplay from "../components/PersonDisplay.vue";
import SignatureBadge from "../components/SignatureBadge.vue";
import Icon from "../components/ui/Icon.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import Skeleton from "../components/ui/Skeleton.vue";
import { signedIn } from "../lib/account.ts";
import { client } from "../lib/client.ts";
import { personLabel, requestPeople } from "../lib/people.ts";
import { useRepo } from "../lib/repo-context.ts";
import { useResource } from "../lib/resource.ts";
import { settings } from "../lib/settings.ts";
import { canRecheck, labelDate, signatureLabel } from "../lib/signatures.ts";
import { absoluteTime } from "../lib/time.ts";
import { notify } from "../lib/toast.ts";
import type { GitPerson } from "../lib/types.ts";

const { org, repo } = useRepo();
const route = useRoute();
const sha = computed(() => String(route.params.sha));

const result = useResource(
	(signal) => client.commitSignature(org.value, repo.value, sha.value, signal),
	[org, repo, sha, () => settings.sandbox],
);
const stored = computed(() => {
	const value = result.data.value;
	return value && value.status !== "pending" ? value : null;
});
watchEffect(() => {
	const did = result.data.value?.did;
	if (did) requestPeople([did]);
});

const label = computed(() => (result.data.value ? signatureLabel(result.data.value, personLabel) : null));
const recheckable = computed(() =>
	result.data.value
		? canRecheck(result.data.value, signedIn.value ? { did: signedIn.value.did, owner: signedIn.value.owner } : null)
		: false
);
const person = (who: GitPerson | null) => (who ? `${who.name} <${who.email}>` : "—");

const rechecking = ref(false);
const recheck = async () => {
	rechecking.value = true;
	try {
		await client.recheckSignature(org.value, repo.value, sha.value);
		notify("Queued to be checked again", "success");
		await result.reload();
	} catch (cause) {
		notify(cause instanceof Error ? cause.message : String(cause), "danger");
	} finally {
		rechecking.value = false;
	}
};

const cardStyles = card();
const styles = {
	head: css({ display: "grid", gap: "2", mb: "4" }),
	sha: css({ fontFamily: "mono", fontSize: "sm", color: "fg.muted", wordBreak: "break-all" }),
	body: css({ display: "grid", gap: "4" }),
	dl: css({
		display: "grid",
		gridTemplateColumns: { base: "1fr", sm: "max-content 1fr" },
		gap: "2 4",
		fontSize: "sm",
		"& dt": { color: "fg.muted" },
		"& dd": { wordBreak: "break-word" },
	}),
	mono: css({ fontFamily: "mono", fontSize: "xs" }),
	detail: css({ fontSize: "sm" }),
	notes: css({ display: "grid", gap: "1", fontSize: "sm", color: "fg.muted", listStyle: "disc", pl: "5" }),
	actions: css({ display: "flex", gap: "2", flexWrap: "wrap" }),
};
</script>

<template>
	<div :class="styles.head">
		<h2>Commit signature</h2>
		<div :class="styles.sha">{{ sha }}</div>
	</div>
	<ProblemAlert v-if="result.error.value" :error="result.error.value" @retry="result.reload()" />
	<Skeleton v-else-if="!result.data.value" :rows="4" />
	<section v-else :class="cardStyles.root">
		<div :class="[cardStyles.body, styles.body]">
			<div><SignatureBadge :result="result.data.value" /></div>
			<p v-if="label?.detail" :class="styles.detail" data-detail>{{ label.detail }}</p>

			<dl v-if="stored" :class="styles.dl">
				<dt>Verified signer</dt>
				<dd>
					<PersonDisplay v-if="stored.status === 'verified' && stored.did" :did="stored.did" />
					<template v-else>—</template>
				</dd>
				<dt>Claimed author</dt>
				<dd>{{ person(stored.author) }}</dd>
				<dt>Claimed committer</dt>
				<dd>{{ person(stored.committer) }}</dd>
				<template v-if="stored.record_uri">
					<dt>Signature record</dt>
					<dd>
						<code :class="styles.mono" data-record-uri>{{ stored.record_uri }}</code>
						<div v-if="stored.record_rev && stored.verified_at && stored.did">
							in {{ personLabel(stored.did) }}'s repository since
							<time :title="absoluteTime(stored.verified_at)">{{ labelDate(stored.verified_at) }}</time>
							(revision <code :class="styles.mono">{{ stored.record_rev }}</code>)
						</div>
					</dd>
				</template>
			</dl>
			<dl v-else-if="result.data.value.status === 'pending'" :class="styles.dl">
				<dt>Attempts</dt>
				<dd>{{ result.data.value.attempts }}</dd>
				<template v-if="result.data.value.next_at">
					<dt>Next attempt</dt>
					<dd>{{ absoluteTime(result.data.value.next_at) }}</dd>
				</template>
			</dl>

			<ul v-if="stored && stored.annotations.length" :class="styles.notes" data-annotations>
				<li v-for="note in stored.annotations" :key="note">{{ note }}</li>
			</ul>

			<div :class="styles.actions">
				<a v-if="stored?.proof_url" :href="stored.proof_url" :class="button({ size: 'sm' })" download>
					<Icon name="download" :size="14" /> Download the proof
				</a>
				<button v-if="recheckable" type="button" :class="button({ size: 'sm' })" :disabled="rechecking" @click="recheck">
					<Icon name="refresh" :size="14" /> Check again
				</button>
			</div>
		</div>
	</section>
</template>

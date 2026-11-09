<script setup lang="ts">
/**
 * Access tokens for scripts and agents (design D5): create, list and revoke. A new token's
 * `tartan_pat_` secret is shown once, in a dialog with a copy button; the forge keeps only its
 * hash, and closing the dialog forgets it here too. A token's rights are its scopes
 * intersected with the person's, so a person who may only read is offered only `read`.
 */
import { computed, reactive, ref, useId } from "vue";
import { css } from "styled-system/css";
import { badge, button, card, field } from "styled-system/recipes";
import { hasRight } from "../../lib/account.ts";
import { client } from "../../lib/client.ts";
import { useResource } from "../../lib/resource.ts";
import { settings } from "../../lib/settings.ts";
import { absoluteTime, relativeTime } from "../../lib/time.ts";
import { notify } from "../../lib/toast.ts";
import type { NewToken, Right } from "../../lib/types.ts";
import CopyButton from "../ui/CopyButton.vue";
import Icon from "../ui/Icon.vue";
import ProblemAlert from "../ui/ProblemAlert.vue";
import Skeleton from "../ui/Skeleton.vue";

const id = useId();
const tokens = useResource((signal) => client.tokens(signal), [() => settings.sandbox]);

/** The scopes this person may usefully give a token. */
const offered = computed<Right[]>(() => (["read", "write", "admin"] as const).filter((r) => hasRight(r)));

const draft = reactive({ name: "", scopes: ["read"] as Right[], repos: "", days: 30 });
const creating = ref(false);
const createError = ref<unknown>();
/** The secret of the token just created, until its dialog closes. */
const created = ref<NewToken | null>(null);

const create = async () => {
	if (creating.value) return;
	creating.value = true;
	createError.value = undefined;
	try {
		const repos = draft.repos.split(/[\s,]+/).map((r) => r.trim()).filter(Boolean);
		created.value = await client.createToken({
			name: draft.name.trim(),
			scopes: draft.scopes.filter((s) => offered.value.includes(s)),
			repos: repos.length ? repos : null,
			expires_in_days: draft.days,
		});
		draft.name = "";
		draft.repos = "";
		await tokens.reload({ quiet: true });
	} catch (cause) {
		createError.value = cause;
	} finally {
		creating.value = false;
	}
};

const closeSecret = () => {
	created.value = null;
};

const revoke = async (tokenId: string, name: string) => {
	try {
		await client.revokeToken(tokenId);
		notify(`Revoked ${name}`, "success");
		await tokens.reload({ quiet: true });
	} catch (cause) {
		notify(cause instanceof Error ? cause.message : String(cause), "danger");
	}
};

const live = computed(() => (tokens.data.value ?? []).filter((t) => t.revoked_at === null));

const cardStyles = card();
const styles = {
	body: css({ display: "grid", gap: "4" }),
	row: css({ display: "grid", gap: "1.5" }),
	label: css({ fontSize: "sm", fontWeight: "semibold" }),
	help: css({ fontSize: "xs", color: "fg.muted", textWrap: "pretty" }),
	scopes: css({ display: "flex", gap: "3", flexWrap: "wrap", fontSize: "sm" }),
	inline: css({ display: "flex", gap: "2", alignItems: "center", flexWrap: "wrap" }),
	list: css({ display: "grid", gap: "2" }),
	item: css({
		display: "flex",
		alignItems: "center",
		gap: "3",
		flexWrap: "wrap",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "md",
		px: "3",
		py: "2",
	}),
	name: css({ fontWeight: "medium", flex: 1, minW: "32" }),
	meta: css({ fontSize: "xs", color: "fg.muted" }),
	secret: css({
		display: "grid",
		gap: "2",
		borderWidth: "1px",
		borderColor: "success.border",
		bg: "success.subtle",
		borderRadius: "lg",
		p: "3",
	}),
	code: css({ fontFamily: "mono", fontSize: "sm", wordBreak: "break-all", bg: "bg.surface", p: "2", borderRadius: "md" }),
	number: css({ w: "24 !important" }),
};
</script>

<template>
	<section :class="cardStyles.root" aria-labelledby="tokens-title">
		<div :class="cardStyles.header"><h2 id="tokens-title" :class="cardStyles.title">Access tokens</h2></div>
		<div :class="[cardStyles.body, styles.body]">
			<p :class="styles.help">
				For scripts, CI and agents: send one as <code>Authorization: Bearer …</code>, or use it with
				<code>tt login --with-token</code>. A token acts for you, with at most your rights.
			</p>

			<div v-if="created" :class="styles.secret" role="dialog" aria-label="New token" data-token-secret>
				<strong>{{ created.token.name }} was created.</strong>
				<div :class="styles.inline">
					<code :class="styles.code">{{ created.secret }}</code>
					<CopyButton :text="created.secret" label="Copy the token" />
				</div>
				<p :class="styles.help">
					Copy it now: this is the only time it is shown, and it cannot be retrieved later. The forge
					keeps only its hash.
				</p>
				<div>
					<button type="button" :class="button({ size: 'sm' })" @click="closeSecret">Done</button>
				</div>
			</div>

			<form :class="styles.body" @submit.prevent="create">
				<div :class="styles.row">
					<label :for="`${id}-name`" :class="styles.label">Name</label>
					<input :id="`${id}-name`" v-model="draft.name" :class="field()" required maxlength="100" placeholder="ci" />
				</div>
				<fieldset :class="styles.row">
					<legend :class="styles.label">Scopes</legend>
					<div :class="styles.scopes" data-scopes>
						<label v-for="scope in offered" :key="scope">
							<input v-model="draft.scopes" type="checkbox" :value="scope" /> {{ scope }}
						</label>
					</div>
					<p v-if="!hasRight('write')" :class="styles.help">
						You may only read on this forge, so a token can only read too.
					</p>
				</fieldset>
				<div :class="styles.row">
					<label :for="`${id}-repos`" :class="styles.label">Repositories (optional)</label>
					<input :id="`${id}-repos`" v-model="draft.repos" :class="field({ mono: true })" placeholder="acme/api, acme/*" />
					<p :class="styles.help">Limit it to <code>org/repo</code> or <code>org/*</code>; empty means every repository.</p>
				</div>
				<div :class="styles.row">
					<label :for="`${id}-days`" :class="styles.label">Expires after, in days</label>
					<input :id="`${id}-days`" v-model.number="draft.days" type="number" min="1" max="365" :class="[field(), styles.number]" />
				</div>
				<ProblemAlert v-if="createError" :error="createError" compact no-retry />
				<div>
					<button type="submit" :class="button({ variant: 'solid', size: 'sm' })" :disabled="creating || draft.scopes.length === 0">
						<Icon name="key" :size="14" /> {{ creating ? "Creating…" : "Create token" }}
					</button>
				</div>
			</form>

			<ProblemAlert v-if="tokens.error.value" :error="tokens.error.value" compact @retry="tokens.reload()" />
			<Skeleton v-else-if="!tokens.data.value" :rows="2" />
			<p v-else-if="live.length === 0" :class="styles.help">No tokens.</p>
			<ul v-else :class="styles.list" role="list" data-token-list>
				<li v-for="token in live" :key="token.id" :class="styles.item">
					<span :class="styles.name">{{ token.name }}</span>
					<span v-for="scope in token.scopes" :key="scope" :class="badge()">{{ scope }}</span>
					<span v-if="token.repos" :class="styles.meta">{{ token.repos.join(", ") }}</span>
					<span :class="styles.meta" :title="absoluteTime(token.expires_at)">expires {{ relativeTime(token.expires_at) }}</span>
					<span :class="styles.meta">{{ token.last_used_at ? `used ${relativeTime(token.last_used_at)}` : "never used" }}</span>
					<button type="button" :class="button({ size: 'xs', variant: 'danger' })" @click="revoke(token.id, token.name)">Revoke</button>
				</li>
			</ul>
		</div>
	</section>
</template>

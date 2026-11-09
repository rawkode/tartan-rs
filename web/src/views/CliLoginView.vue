<script setup lang="ts">
/**
 * Approves `tt login` (design D16). `tt` opens this page with its loopback port, a `state`,
 * a PKCE challenge and its device name. Approve asks the forge for a one-time code and goes to
 * the loopback address the forge answers, always `http://127.0.0.1:<port>`; Cancel goes there
 * with `error=access_denied`. The router has already sent a visitor without a session to sign
 * in and back.
 */
import { computed, ref } from "vue";
import { useRoute } from "vue-router";
import { css } from "styled-system/css";
import { button, card } from "styled-system/recipes";
import Icon from "../components/ui/Icon.vue";
import PageHeader from "../components/ui/PageHeader.vue";
import ProblemAlert from "../components/ui/ProblemAlert.vue";
import { signedIn } from "../lib/account.ts";
import { client } from "../lib/client.ts";
import { navigation } from "../lib/navigate.ts";

const route = useRoute();
const one = (value: unknown): string => (typeof value === "string" ? value : "");

const port = computed(() => {
	const text = one(route.query.port);
	const n = /^\d{1,5}$/.test(text) ? Number(text) : NaN;
	return n >= 1024 && n <= 65535 ? n : null;
});
const state = computed(() => one(route.query.state));
const challenge = computed(() => one(route.query.challenge));
const device = computed(() => one(route.query.device).slice(0, 64) || "this device");
const valid = computed(() =>
	port.value !== null && /^[A-Za-z0-9_-]{43}$/.test(state.value) && /^[A-Za-z0-9_-]{43}$/.test(challenge.value)
);
const forge = computed(() => globalThis.location?.host ?? "this forge");
const who = computed(() => (signedIn.value?.handle ? `@${signedIn.value.handle}` : signedIn.value?.did ?? ""));

const busy = ref(false);
const error = ref<unknown>();

const approve = async () => {
	if (!valid.value || busy.value || port.value === null) return;
	busy.value = true;
	error.value = undefined;
	try {
		const { redirect } = await client.approveCli({
			port: port.value,
			state: state.value,
			challenge: challenge.value,
			device: one(route.query.device).slice(0, 64),
		});
		navigation.assign(redirect);
	} catch (cause) {
		error.value = cause;
	} finally {
		busy.value = false;
	}
};

const cancel = () => {
	if (port.value === null) return;
	const query = new URLSearchParams({ error: "access_denied", state: state.value });
	navigation.assign(`http://127.0.0.1:${port.value}/forge?${query}`);
};

const cardStyles = card();
const styles = {
	box: css({ maxW: "xl" }),
	body: css({ display: "grid", gap: "4" }),
	question: css({ fontSize: "lg", textWrap: "pretty" }),
	help: css({ fontSize: "sm", color: "fg.muted", textWrap: "pretty" }),
	actions: css({ display: "flex", gap: "2" }),
};
</script>

<template>
	<PageHeader title="Approve tt" description="Let the tt command line act as you on this forge." />
	<section :class="[cardStyles.root, styles.box]">
		<div :class="[cardStyles.body, styles.body]">
			<template v-if="valid">
				<p :class="styles.question" data-question>
					Allow <code>tt</code> on <strong>{{ device }}</strong> to act as <strong>{{ who }}</strong> on
					{{ forge }}?
				</p>
				<p :class="styles.help">
					It gets a session of its own, listed under Settings, which you can end at any time. Only approve
					a request you started with <code>tt login</code>.
				</p>
				<ProblemAlert v-if="error" :error="error" compact no-retry />
				<div :class="styles.actions">
					<button type="button" :class="button({ variant: 'solid' })" :disabled="busy" @click="approve">
						<Icon name="check" :size="14" /> Approve
					</button>
					<button type="button" :class="button()" :disabled="busy" @click="cancel">Cancel</button>
				</div>
			</template>
			<p v-else role="alert" :class="styles.help">
				This approval link is incomplete. Run <code>tt login</code> again and open the address it prints.
			</p>
		</div>
	</section>
</template>

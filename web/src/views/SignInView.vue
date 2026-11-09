<script setup lang="ts">
/**
 * Sign in with an Atmosphere account (design D1, D15). The handle goes to the forge as a
 * top-level form `POST /-/auth/login`, so the browser follows the forge's redirect to the
 * person's authorization server and keeps the cookie that binds the sign-in to it.
 *
 * A failed sign-in comes back here with `?problem=<code>` and maybe `&outcome=<id>`. Only the
 * code and a known account status are read from the URL; hosts and server messages are read
 * once from `POST /-/auth/outcome`, which answers only the browser that ran the flow.
 * `return_to` survives every round trip, "try again" included.
 */
import { computed, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { button, card, field } from "styled-system/recipes";
import Icon from "../components/ui/Icon.vue";
import Logo from "../components/ui/Logo.vue";
import { devIdentity, signedIn } from "../lib/account.ts";
import { client, loadAccount } from "../lib/client.ts";
import { type OutcomeDetails, problemText, safeReturnTo } from "../lib/signin.ts";

const route = useRoute();
const router = useRouter();

const one = (value: unknown): string | undefined =>
	typeof value === "string" ? value : Array.isArray(value) && typeof value[0] === "string" ? value[0] : undefined;

const problem = computed(() => one(route.query.problem));
const status = computed(() => one(route.query.status));
const outcome = computed(() => one(route.query.outcome));
const returnTo = computed(() => safeReturnTo(one(route.query.return_to)));

const details = ref<OutcomeDetails | null>(null);
const text = computed(() => (problem.value ? problemText(problem.value, status.value, details.value) : null));

/** Reads the failure's details once; anything but an answer leaves the generic words. */
const readOutcome = async () => {
	details.value = null;
	if (!problem.value || !outcome.value) return;
	try {
		details.value = await client.outcome(outcome.value, problem.value);
	} catch {
		details.value = null;
	}
};

onMounted(() => {
	void loadAccount();
	void readOutcome();
});
watch([problem, outcome], () => void readOutcome());

const handle = ref("");
const tryAgain = () => router.replace({ name: "sign-in", query: { return_to: returnTo.value } });

const cardStyles = card();
const styles = {
	page: css({ display: "grid", placeItems: "center", minH: "60vh" }),
	box: css({ w: "full", maxW: "md" }),
	head: css({ display: "flex", alignItems: "center", gap: "3", mb: "4" }),
	title: css({ fontSize: "xl", fontWeight: "semibold" }),
	body: css({ display: "grid", gap: "4" }),
	help: css({ fontSize: "sm", color: "fg.muted", textWrap: "pretty" }),
	label: css({ fontSize: "sm", fontWeight: "semibold" }),
	row: css({ display: "grid", gap: "1.5" }),
	problem: css({
		display: "grid",
		gap: "1",
		borderWidth: "1px",
		borderColor: "warning.border",
		bg: "warning.subtle",
		borderRadius: "lg",
		p: "3",
	}),
	problemTitle: css({ fontWeight: "semibold", color: "warning.fg" }),
	server: css({ fontFamily: "mono", fontSize: "xs", wordBreak: "break-word", color: "fg.muted" }),
	dev: css({ borderTopWidth: "1px", borderColor: "border.default", pt: "4", display: "grid", gap: "2" }),
};
</script>

<template>
	<div :class="styles.page">
		<div :class="styles.box">
			<div :class="styles.head">
				<Logo :size="32" />
				<h1 :class="styles.title">Sign in to Tartan</h1>
			</div>
			<section :class="cardStyles.root">
				<div :class="[cardStyles.body, styles.body]">
					<div v-if="text" :class="styles.problem" role="alert" data-problem>
						<div :class="styles.problemTitle">{{ text.title }}</div>
						<div>{{ text.body }}</div>
						<div v-if="text.server" :class="styles.server" data-server-text>{{ text.server }}</div>
						<div>
							<button type="button" :class="button({ size: 'sm' })" @click="tryAgain">
								<Icon name="refresh" :size="14" /> Try again
							</button>
						</div>
					</div>

					<template v-if="signedIn && !text">
						<p :class="styles.help">
							You are signed in as <strong>{{ signedIn.handle ? `@${signedIn.handle}` : signedIn.did }}</strong>.
						</p>
						<a :href="returnTo" :class="button({ variant: 'solid' })">Continue</a>
					</template>

					<form v-else method="post" action="/-/auth/login" :class="styles.body">
						<div :class="styles.row">
							<label for="handle" :class="styles.label">Handle or DID</label>
							<input
								id="handle"
								v-model="handle"
								name="handle"
								:class="field()"
								placeholder="alice.bsky.social"
								autocomplete="username"
								autocapitalize="off"
								spellcheck="false"
								required
							/>
							<p :class="styles.help">
								Your Atmosphere account, such as a Bluesky handle. You approve the sign-in at your own
								server; the forge never sees your password.
							</p>
						</div>
						<input type="hidden" name="return_to" :value="returnTo" />
						<button type="submit" :class="button({ variant: 'solid' })">
							<Icon name="user" :size="14" /> Sign in
						</button>
					</form>

					<form v-if="devIdentity && !signedIn" method="post" action="/-/auth/login" :class="styles.dev" data-dev-sign-in>
						<p :class="styles.help">This forge runs its development identity network.</p>
						<input type="hidden" name="handle" value="alice.test" />
						<input type="hidden" name="return_to" :value="returnTo" />
						<button type="submit" :class="button()">Sign in as @alice.test</button>
					</form>
				</div>
			</section>
		</div>
	</div>
</template>

<script setup lang="ts">
/**
 * The header's account menu: who is signed in, whether they own the forge, and signing out.
 * It shows only what `GET /-/api/me` answered, so it makes no request of its own.
 */
import { RouterLink, useRouter } from "vue-router";
import { css } from "styled-system/css";
import { badge, button } from "styled-system/recipes";
import { signedIn } from "../lib/account.ts";
import { signOut } from "../lib/client.ts";
import { notify } from "../lib/toast.ts";
import Icon from "./ui/Icon.vue";

const router = useRouter();

const leave = async () => {
	try {
		await signOut();
		await router.push({ name: "sign-in" });
	} catch (error) {
		notify(error instanceof Error ? error.message : String(error), "danger");
	}
};

const styles = {
	root: css({ position: "relative", "& > summary": { listStyle: "none" }, "& > summary::-webkit-details-marker": { display: "none" } }),
	summary: css({ display: "inline-flex", alignItems: "center", gap: "1.5", cursor: "pointer" }),
	panel: css({
		position: "absolute",
		right: 0,
		mt: "2",
		minW: "64",
		bg: "bg.surface",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "lg",
		boxShadow: "lg",
		p: "3",
		display: "grid",
		gap: "2",
		zIndex: 30,
	}),
	did: css({ fontFamily: "mono", fontSize: "xs", color: "fg.muted", wordBreak: "break-all" }),
	row: css({ display: "flex", alignItems: "center", gap: "2" }),
};
</script>

<template>
	<details v-if="signedIn" :class="styles.root" data-account-menu>
		<summary :class="[button({ variant: 'ghost', size: 'sm' }), styles.summary]" aria-label="Account">
			<Icon name="user" :size="14" />
			<span>{{ signedIn.handle ? `@${signedIn.handle}` : "Account" }}</span>
			<span v-if="signedIn.owner" :class="badge({ tone: 'accent' })" data-owner-badge>owner</span>
		</summary>
		<div :class="styles.panel">
			<div :class="styles.row"><strong>{{ signedIn.handle ? `@${signedIn.handle}` : "Signed in" }}</strong></div>
			<div :class="styles.did">{{ signedIn.did }}</div>
			<RouterLink :to="{ name: 'settings' }" :class="button({ size: 'sm', variant: 'ghost' })">
				<Icon name="settings" :size="14" /> Settings, tokens and sessions
			</RouterLink>
			<button type="button" :class="button({ size: 'sm' })" @click="leave">
				<Icon name="logout" :size="14" /> Sign out
			</button>
		</div>
	</details>
</template>

<script setup lang="ts">
/**
 * The person's own sessions (design D6): browser sessions and `tt`'s CLI sessions with their
 * device names. Ending the current one signs this browser out.
 */
import { useRouter } from "vue-router";
import { css } from "styled-system/css";
import { badge, button, card } from "styled-system/recipes";
import { setAccount } from "../../lib/account.ts";
import { client } from "../../lib/client.ts";
import { useResource } from "../../lib/resource.ts";
import { settings } from "../../lib/settings.ts";
import { absoluteTime, relativeTime } from "../../lib/time.ts";
import { notify } from "../../lib/toast.ts";
import type { SessionInfo } from "../../lib/types.ts";
import ProblemAlert from "../ui/ProblemAlert.vue";
import Skeleton from "../ui/Skeleton.vue";

const router = useRouter();
const sessions = useResource((signal) => client.sessions(signal), [() => settings.sandbox]);

const end = async (session: SessionInfo) => {
	try {
		await client.endSession(session.id);
		if (session.current) {
			setAccount({ anonymous: true, dev_identity: false });
			await router.push({ name: "sign-in" });
			return;
		}
		notify(session.kind === "cli" ? `Ended tt's session on ${session.device ?? "a device"}` : "Ended the session", "success");
		await sessions.reload({ quiet: true });
	} catch (cause) {
		notify(cause instanceof Error ? cause.message : String(cause), "danger");
	}
};

const cardStyles = card();
const styles = {
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
	device: css({ fontWeight: "medium", flex: 1, minW: "32" }),
	meta: css({ fontSize: "xs", color: "fg.muted" }),
	help: css({ fontSize: "xs", color: "fg.muted" }),
};
</script>

<template>
	<section :class="cardStyles.root" aria-labelledby="sessions-title">
		<div :class="cardStyles.header"><h2 id="sessions-title" :class="cardStyles.title">Sessions</h2></div>
		<div :class="cardStyles.body">
			<ProblemAlert v-if="sessions.error.value" :error="sessions.error.value" compact @retry="sessions.reload()" />
			<Skeleton v-else-if="!sessions.data.value" :rows="2" />
			<p v-else-if="sessions.data.value.length === 0" :class="styles.help">No sessions.</p>
			<ul v-else :class="styles.list" role="list" data-session-list>
				<li v-for="session in sessions.data.value" :key="session.id" :class="styles.item">
					<span :class="badge({ tone: session.kind === 'cli' ? 'accent' : 'neutral' })">{{ session.kind === "cli" ? "tt (CLI)" : "Browser" }}</span>
					<span :class="styles.device">{{ session.device ?? "Unknown device" }}</span>
					<span v-if="session.current" :class="badge({ tone: 'success' })">this browser</span>
					<span :class="styles.meta" :title="absoluteTime(session.created_at)">signed in {{ relativeTime(session.created_at) }}</span>
					<span :class="styles.meta" :title="absoluteTime(session.last_used_at)">used {{ relativeTime(session.last_used_at) }}</span>
					<button type="button" :class="button({ size: 'xs', variant: 'danger' })" @click="end(session)">
						{{ session.current ? "Sign out" : "End" }}
					</button>
				</li>
			</ul>
		</div>
	</section>
</template>

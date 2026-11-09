/**
 * The sign-in page's words (design D1, *Failures*). A failed sign-in lands on
 * `/#/-/sign-in?problem=<code>` with, for some codes, an `outcome` id. Only fixed-list values
 * come from the URL: the code and, for `account_inactive`, a known status. Hosts and server
 * messages come only from `POST /-/auth/outcome`, which answers only the browser that ran the
 * flow, so a crafted link can never put its own text on the page.
 */

/** Account statuses with their own words; any other `status` reads as "not active". */
export const KNOWN_STATUSES = [
	"deactivated",
	"takendown",
	"suspended",
	"deleted",
	"throttled",
	"desynchronized",
] as const;

/** The longest server text the page shows. */
export const DETAIL_MAX = 300;

/** What the outcome record may hold. Every value is shown as text. */
export interface OutcomeDetails {
	host?: string;
	message?: string;
	error?: string;
	error_description?: string;
}

export interface ProblemText {
	title: string;
	body: string;
	/** Server text to show verbatim (escaped), such as an `error_description`. */
	server?: string;
}

const clip = (text: string | undefined): string | undefined => {
	const trimmed = text?.trim();
	if (!trimmed) return undefined;
	return trimmed.length > DETAIL_MAX ? `${trimmed.slice(0, DETAIL_MAX - 1)}…` : trimmed;
};

const yourServer = (details: OutcomeDetails | null): string => {
	const host = clip(details?.host);
	return host ? host : "your server";
};

const inactive = (status: string | undefined, details: OutcomeDetails | null): ProblemText => {
	const known = (KNOWN_STATUSES as readonly string[]).includes(status ?? "") ? status : undefined;
	const at = details?.host ? ` (${clip(details.host)})` : "";
	switch (known) {
		case "deactivated":
			return {
				title: "Your account is deactivated",
				body: `Reactivate it at your PDS${at}, then sign in again.`,
			};
		case "takendown":
		case "suspended":
			return {
				title: "Your account is suspended",
				body: `Contact your hosting provider${at}.`,
			};
		case "deleted":
			return { title: "This account no longer exists", body: "It cannot sign in." };
		default:
			return {
				title: "Your account is not active",
				body: `It is not active at its PDS${at}.`,
			};
	}
};

/**
 * The words for a problem code. `status` is the URL's `status` (used only when it is a known
 * one); `details` is what `POST /-/auth/outcome` answered, or null.
 */
export const problemText = (
	code: string,
	status: string | undefined,
	details: OutcomeDetails | null,
): ProblemText => {
	switch (code) {
		case "handle_not_found":
			return {
				title: "That account was not found",
				body: "No account has that handle or DID. Check the spelling and try again.",
			};
		case "server_unreachable":
			return {
				title: "Your server could not be reached",
				body: `Sign-in could not reach ${yourServer(details)}. Try again in a moment.`,
			};
		case "client_rejected": {
			const server = [clip(details?.error), clip(details?.error_description)].filter(Boolean).join(": ");
			return {
				title: "Your server refused this forge",
				body: `${yourServer(details)} refused this forge's sign-in configuration. ` +
					"Please tell the forge's operator, so they can fix it.",
				...(server ? { server } : {}),
			};
		}
		case "denied": {
			const server = clip(details?.error_description);
			return {
				title: "Sign-in was cancelled",
				body: "The sign-in was cancelled or refused at your server.",
				...(server ? { server } : {}),
			};
		}
		case "expired":
			return { title: "Sign-in took too long", body: "The sign-in expired before it finished. Try again." };
		case "browser_mismatch":
			return {
				title: "Sign-in finished in another browser",
				body: "Finish signing in in the browser where you started. Try again.",
			};
		case "wrong_issuer":
			return {
				title: "Sign-in could not be confirmed",
				body: "The server that answered does not speak for your account. Try again.",
			};
		case "wrong_subject":
			return {
				title: "Another account signed in",
				body: "Your server signed in a different account from the one you asked for. Try again.",
			};
		case "missing_scope":
			return {
				title: "Sign-in was not granted",
				body: "Your server did not grant the permission sign-in needs. Try again.",
			};
		case "account_inactive":
			return inactive(status, details);
		case "rate_limited":
			return {
				title: "Too many sign-in attempts",
				body: "Too many sign-ins came from your address. Try again in a few minutes.",
			};
		case "busy":
			return { title: "The forge is busy", body: "Too many sign-ins are in progress. Try again shortly." };
		default:
			return { title: "Sign-in did not finish", body: "Try again." };
	}
};

/**
 * `return_to` as the forge accepts it (design D15): a path on this origin such as
 * `/#/acme/api/issues`, starting with exactly one `/`, not `//` or `/\`, without control
 * characters. Anything else is `/`.
 */
export const safeReturnTo = (value: unknown): string => {
	if (typeof value !== "string") return "/";
	const control = /[\u0000-\u001f\u007f]/;
	if (!value.startsWith("/") || value.startsWith("//") || value.startsWith("/\\") || control.test(value)) {
		return "/";
	}
	try {
		return new URL(value, "http://forge.invalid").host === "forge.invalid" ? value : "/";
	} catch {
		return "/";
	}
};

/** The `return_to` for a page of this app: its hash route. */
export const returnToFor = (fullPath: string): string => `/#${fullPath}`;

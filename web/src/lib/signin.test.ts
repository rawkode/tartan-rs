import { describe, expect, test } from "vitest";
import { problemText, returnToFor, safeReturnTo } from "./signin.ts";

describe("problem messages", () => {
	test.each([
		["handle_not_found", "That account was not found", /Check the spelling/],
		["server_unreachable", "Your server could not be reached", /could not reach your server/],
		["client_rejected", "Your server refused this forge", /tell the forge's operator/],
		["denied", "Sign-in was cancelled", /cancelled or refused/],
		["expired", "Sign-in took too long", /expired/],
		["browser_mismatch", "Sign-in finished in another browser", /browser where you started/],
		["wrong_issuer", "Sign-in could not be confirmed", /does not speak for your account/],
		["wrong_subject", "Another account signed in", /different account/],
		["missing_scope", "Sign-in was not granted", /did not grant/],
		["rate_limited", "Too many sign-in attempts", /in a few minutes/],
		["busy", "The forge is busy", /shortly/],
		["something_new", "Sign-in did not finish", /Try again/],
	])("%s", (code, title, body) => {
		const text = problemText(code, undefined, null);
		expect(text.title).toBe(title);
		expect(text.body).toMatch(body);
		expect(text.server).toBeUndefined();
	});

	test.each([
		["deactivated", "Your account is deactivated", "Reactivate it at your PDS (pds.example), then sign in again."],
		["takendown", "Your account is suspended", "Contact your hosting provider (pds.example)."],
		["suspended", "Your account is suspended", "Contact your hosting provider (pds.example)."],
		["deleted", "This account no longer exists", "It cannot sign in."],
		["throttled", "Your account is not active", "It is not active at its PDS (pds.example)."],
		["desynchronized", "Your account is not active", "It is not active at its PDS (pds.example)."],
		["inactive", "Your account is not active", "It is not active at its PDS (pds.example)."],
		["made-up", "Your account is not active", "It is not active at its PDS (pds.example)."],
	])("account_inactive with status %s", (status, title, body) => {
		const text = problemText("account_inactive", status, { host: "pds.example" });
		expect(text).toEqual({ title, body });
	});

	test("account_inactive names no host without an outcome record", () => {
		expect(problemText("account_inactive", "deactivated", null).body).toBe(
			"Reactivate it at your PDS, then sign in again.",
		);
	});

	test("server text and hosts come only from the outcome details, clipped", () => {
		const long = "x".repeat(400);
		const text = problemText("client_rejected", undefined, {
			host: "pds.example",
			error: "invalid_client_metadata",
			error_description: long,
		});
		expect(text.body).toContain("pds.example refused");
		expect(text.server!.startsWith("invalid_client_metadata: ")).toBe(true);
		expect(text.server!.length).toBeLessThanOrEqual("invalid_client_metadata: ".length + 300);
		expect(problemText("server_unreachable", undefined, { host: "pds.example" }).body).toContain("pds.example");
		expect(problemText("denied", undefined, { error_description: "User said no" }).server).toBe("User said no");
	});
});

describe("return_to", () => {
	test.each([
		["/#/acme/api/issues", "/#/acme/api/issues"],
		["/#/acme/api/issues?x=1", "/#/acme/api/issues?x=1"],
		["/", "/"],
		["//evil.example/", "/"],
		["/\\evil.example", "/"],
		["https://evil.example", "/"],
		["", "/"],
		["x", "/"],
		["/a\nb", "/"],
		[undefined, "/"],
	])("%o is %o", (value, expected) => {
		expect(safeReturnTo(value)).toBe(expected);
	});

	test("a page's return_to is its hash route", () => {
		expect(returnToFor("/acme/api/issues?x=1")).toBe("/#/acme/api/issues?x=1");
	});
});

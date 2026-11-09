import { describe, expect, test } from "vitest";
import {
	changedProperties,
	formatValue,
	fromMinorUnits,
	initialValues,
	isDate,
	isHttpUrl,
	minorDigits,
	propertyTitle,
	toMinorUnits,
	validateValue,
} from "./properties.ts";
import type { PropertyDef } from "./types.ts";

const select: PropertyDef = {
	name: "status",
	kind: "select",
	options: ["todo", "done"],
	labels: { todo: "To do" },
	default: "todo",
};

describe("validation mirrors tartan.dev/types", () => {
	test.each<[PropertyDef, unknown, boolean]>([
		[{ name: "a", kind: "shorttext" }, "fine", true],
		[{ name: "a", kind: "shorttext" }, "two\nlines", false],
		[{ name: "a", kind: "shorttext", maxLength: 3 }, "four", false],
		[{ name: "a", kind: "longtext" }, "# Heading\n\nbody", true],
		[{ name: "a", kind: "number", min: 0, max: 10 }, 11, false],
		[{ name: "a", kind: "number", min: 0, max: 10 }, 2.5, true],
		[{ name: "a", kind: "date" }, "2026-02-29", false],
		[{ name: "a", kind: "date" }, "2028-02-29", true],
		[{ name: "a", kind: "boolean" }, false, true],
		[select, "done", true],
		[select, "doing", false],
		[{ name: "a", kind: "multiselect", options: ["x", "y"] }, ["y", "x"], true],
		[{ name: "a", kind: "multiselect", options: ["x", "y"] }, ["x", "x"], false],
		[{ name: "a", kind: "multiselect", options: ["x", "y"] }, ["z"], false],
		[{ name: "a", kind: "person" }, "admin", true],
		[{ name: "a", kind: "person" }, "two words", false],
		[{ name: "a", kind: "url" }, "https://example.com/x", true],
		[{ name: "a", kind: "url" }, "ftp://example.com", false],
		[{ name: "a", kind: "url" }, "not a url", false],
		[{ name: "a", kind: "money", currency: "GBP" }, 1250, true],
		[{ name: "a", kind: "money", currency: "GBP" }, 12.5, false],
	])("%o accepts %o: %s", (def, value, ok) => {
		expect(validateValue(def, value as never) === null).toBe(ok);
	});

	test("unset values always fit", () => {
		expect(validateValue(select, null)).toBeNull();
		expect(validateValue({ name: "t", kind: "multiselect", options: ["a"] }, [])).toBeNull();
	});
});

describe("money", () => {
	test("converts between major and minor units by the currency's digits", () => {
		expect(minorDigits("GBP")).toBe(2);
		expect(minorDigits("JPY")).toBe(0);
		expect(toMinorUnits("12.5", "GBP")).toBe(1250);
		expect(toMinorUnits("-0.05", "EUR")).toBe(-5);
		expect(toMinorUnits("1,000", "JPY")).toBe(1000);
		expect(toMinorUnits("1.234", "GBP")).toBeNull();
		expect(toMinorUnits("abc", "GBP")).toBeNull();
		expect(fromMinorUnits(1250, "GBP")).toBe("12.50");
		expect(fromMinorUnits(-5, "EUR")).toBe("-0.05");
		expect(fromMinorUnits(1000, "JPY")).toBe("1000");
	});
});

test("titles and formatting use labels", () => {
	expect(propertyTitle({ name: "due_date", kind: "date" })).toBe("Due date");
	expect(propertyTitle({ name: "x", kind: "date", label: "When" })).toBe("When");
	expect(formatValue(select, "todo")).toBe("To do");
	expect(formatValue(select, "done")).toBe("done");
	expect(formatValue({ name: "b", kind: "boolean" }, false)).toBe("No");
	expect(formatValue(select, null)).toBe("—");
});

test("URLs are what Issues accepts", () => {
	expect(isHttpUrl("https://example.com/x")).toBe(true);
	expect(isHttpUrl("http://example.com")).toBe(true);
	expect(isHttpUrl("HTTPS://Example.com/x")).toBe(false);
	expect(isHttpUrl("https:///x")).toBe(false);
	expect(isHttpUrl("https://example.com/a b")).toBe(false);
	expect(isHttpUrl("ftp://example.com")).toBe(false);
	expect(isHttpUrl(`https://example.com/${"a".repeat(2048)}`)).toBe(false);
});

test("dates are calendar days", () => {
	expect(isDate("2026-11-08")).toBe(true);
	expect(isDate("2026-13-01")).toBe(false);
	expect(isDate("26-11-08")).toBe(false);
	// Every year Issues accepts, the first hundred included; 0000 is a leap year.
	expect(isDate("0000-01-01")).toBe(true);
	expect(isDate("0000-02-29")).toBe(true);
	expect(isDate("0099-12-31")).toBe(true);
	expect(isDate("1900-02-29")).toBe(false);
	expect(isDate("2024-02-29")).toBe(true);
});

test("a new issue starts with the defaults", () => {
	expect(initialValues([select, { name: "n", kind: "number" }])).toEqual({ status: "todo" });
});

test("an edit sends only what changed, with null for what it cleared", () => {
	const defs: PropertyDef[] = [
		select,
		{ name: "tags", kind: "multiselect", options: ["a", "b"] },
		{ name: "n", kind: "number" },
	];
	expect(
		changedProperties(defs, { status: "todo", tags: ["a"], n: 1 }, { status: "todo", tags: ["a", "b"], n: null }),
	).toEqual({ tags: ["a", "b"], n: null });
	expect(changedProperties(defs, { tags: [] }, { tags: [] })).toEqual({});
});

/**
 * Working with Issues' typed properties: titles, display, validation and the conversions
 * between what an editor shows and what Issues stores. Validation mirrors
 * `tartan.rawkode.academy/types` so a form can explain a mistake before the extension refuses it; the
 * extension stays the authority.
 */

import type { PropertyDef, PropertyKind, PropertyValue } from "./types.ts";

/** What a person reads for a property: its label, else its name. */
export const propertyTitle = (def: PropertyDef): string => def.label ?? humanise(def.name);

/** Turns `due_date` into `Due date`. */
export const humanise = (name: string): string => {
	const spaced = name.replace(/_/g, " ").trim();
	return spaced.length === 0 ? name : spaced[0]!.toUpperCase() + spaced.slice(1);
};

/** What a person reads for a select option: its label, else the option itself. */
export const optionTitle = (def: Pick<PropertyDef, "labels">, option: string): string =>
	def.labels?.[option] ?? option;

/** Whether a value counts as unset. */
export const isEmpty = (value: PropertyValue | undefined): boolean =>
	value === null || value === undefined || value === "" ||
	(Array.isArray(value) && value.length === 0);

const DATE = /^\d{4}-\d{2}-\d{2}$/;
const CURRENCY = /^[A-Z]{3}$/;

/**
 * Whether `YYYY-MM-DD` names a real calendar day, in any year from 0000 to 9999 (the
 * proleptic Gregorian calendar, as Issues checks it).
 */
export const isDate = (text: string): boolean => {
	if (!DATE.test(text)) return false;
	const [y, m, d] = text.split("-").map(Number) as [number, number, number];
	const leap = y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0);
	const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1];
	return days !== undefined && d >= 1 && d <= days;
};

const URL_MAX_CHARS = 2048;

/**
 * Whether text is an absolute `http://` or `https://` URL as Issues accepts one: the scheme in
 * lower case, a host, no whitespace, at most 2048 characters.
 */
export const isHttpUrl = (text: string): boolean => {
	const rest = text.startsWith("https://")
		? text.slice("https://".length)
		: text.startsWith("http://")
		? text.slice("http://".length)
		: null;
	if (rest === null || /\s/.test(text) || [...text].length > URL_MAX_CHARS) return false;
	const hostEnd = rest.search(/[/?#]/);
	return (hostEnd === -1 ? rest.length : hostEnd) > 0;
};

/** The number of minor-unit digits of a currency (2 for GBP, 0 for JPY). */
export const minorDigits = (currency: string): number => {
	if (!CURRENCY.test(currency)) return 2;
	try {
		return new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions()
			.maximumFractionDigits ?? 2;
	} catch {
		return 2;
	}
};

/** Parses a major-unit amount (`12.50`) into minor units (`1250`); `null` if it is not one. */
export const toMinorUnits = (text: string, currency: string): number | null => {
	const digits = minorDigits(currency);
	const trimmed = text.trim().replace(/,/g, "");
	const pattern = digits === 0 ? /^-?\d+$/ : new RegExp(`^-?\\d+(\\.\\d{1,${digits}})?$`);
	if (!pattern.test(trimmed)) return null;
	const negative = trimmed.startsWith("-");
	const [whole = "0", fraction = ""] = trimmed.replace("-", "").split(".");
	const minor = Number(whole) * 10 ** digits + Number(fraction.padEnd(digits, "0") || "0");
	if (!Number.isSafeInteger(minor)) return null;
	return negative ? -minor : minor;
};

/** Writes minor units as a plain major-unit amount, for an input (`1250` → `12.50`). */
export const fromMinorUnits = (minor: number, currency: string): string => {
	const digits = minorDigits(currency);
	if (digits === 0) return String(minor);
	const negative = minor < 0;
	const abs = Math.abs(minor);
	const whole = Math.floor(abs / 10 ** digits);
	const fraction = String(abs % 10 ** digits).padStart(digits, "0");
	return `${negative ? "-" : ""}${whole}.${fraction}`;
};

/** Formats minor units for display (`1250`, `GBP` → `£12.50`). */
export const formatMoney = (minor: number, currency: string): string => {
	const digits = minorDigits(currency);
	try {
		return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(
			minor / 10 ** digits,
		);
	} catch {
		return `${fromMinorUnits(minor, currency)} ${currency}`;
	}
};

/** Formats `YYYY-MM-DD` as a readable date without shifting it across time zones. */
export const formatDate = (text: string): string => {
	if (!isDate(text)) return text;
	const [y, m, d] = text.split("-").map(Number) as [number, number, number];
	return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" }).format(
		new Date(Date.UTC(y, m - 1, d)),
	);
};

/** A one-line rendering of a value, for lists and cards. Long text is summarised. */
export const formatValue = (def: PropertyDef, value: PropertyValue | undefined): string => {
	if (isEmpty(value)) return "—";
	switch (def.kind) {
		case "boolean":
			return value ? "Yes" : "No";
		case "select":
			return optionTitle(def, String(value));
		case "multiselect":
			return (Array.isArray(value) ? value : [String(value)]).map((v) => optionTitle(def, v))
				.join(", ");
		case "money":
			return typeof value === "number" ? formatMoney(value, def.currency ?? "USD") : String(value);
		case "date":
			return formatDate(String(value));
		case "number":
			return typeof value === "number" ? value.toLocaleString() : String(value);
		case "longtext": {
			const text = String(value).replace(/\s+/g, " ").trim();
			return text.length > 80 ? `${text.slice(0, 79)}…` : text;
		}
		default:
			return String(value);
	}
};

/** Why a value does not fit its property, or `null` when it does. Unset values always fit. */
export const validateValue = (def: PropertyDef, value: PropertyValue | undefined): string | null => {
	if (isEmpty(value)) return null;
	const kind: PropertyKind = def.kind;
	switch (kind) {
		case "shorttext":
		case "longtext": {
			if (typeof value !== "string") return "Must be text.";
			const max = def.maxLength ?? (kind === "shorttext" ? 200 : 20000);
			if (kind === "shorttext" && value.includes("\n")) return "Must be a single line.";
			return value.length > max ? `At most ${max} characters (${value.length} now).` : null;
		}
		case "number": {
			if (typeof value !== "number" || !Number.isFinite(value)) return "Must be a number.";
			if (def.min !== undefined && value < def.min) return `At least ${def.min}.`;
			if (def.max !== undefined && value > def.max) return `At most ${def.max}.`;
			return null;
		}
		case "date":
			return typeof value === "string" && isDate(value) ? null : "Must be a date (YYYY-MM-DD).";
		case "boolean":
			return typeof value === "boolean" ? null : "Must be yes or no.";
		case "select":
			return typeof value === "string" && (def.options ?? []).includes(value)
				? null
				: "Must be one of the options.";
		case "multiselect": {
			if (!Array.isArray(value)) return "Must be a list of options.";
			const unknown = value.filter((v) => !(def.options ?? []).includes(v));
			if (unknown.length > 0) return `Not an option: ${unknown.join(", ")}.`;
			return new Set(value).size === value.length ? null : "Options may appear only once.";
		}
		case "person":
			return typeof value === "string" && value.trim().length > 0 && !/\s/.test(value.trim())
				? null
				: "Must be a principal id, such as admin.";
		case "url":
			return typeof value === "string" && isHttpUrl(value) ? null : "Must be an http(s) URL.";
		case "money":
			return typeof value === "number" && Number.isSafeInteger(value)
				? null
				: "Must be an amount in whole minor units.";
	}
};

/** The values a new issue starts with: each property's default, where it has one. */
export const initialValues = (defs: PropertyDef[]): Record<string, PropertyValue> => {
	const values: Record<string, PropertyValue> = {};
	for (const def of defs) {
		if (def.default !== undefined) {
			values[def.name] = Array.isArray(def.default) ? [...def.default] : def.default;
		}
	}
	return values;
};

const sameValue = (a: PropertyValue | undefined, b: PropertyValue | undefined): boolean => {
	if (isEmpty(a) && isEmpty(b)) return true;
	if (Array.isArray(a) && Array.isArray(b)) {
		return a.length === b.length && a.every((v, i) => v === b[i]);
	}
	return a === b;
};

/**
 * The properties an edit changes, for an update: changed values, and `null` for values the
 * edit cleared. Unchanged properties are left out so concurrent edits of others survive.
 */
export const changedProperties = (
	defs: PropertyDef[],
	before: Record<string, PropertyValue>,
	after: Record<string, PropertyValue>,
): Record<string, PropertyValue> => {
	const changes: Record<string, PropertyValue> = {};
	for (const def of defs) {
		const was = before[def.name];
		const now = after[def.name];
		if (!sameValue(was, now)) changes[def.name] = isEmpty(now) ? null : now ?? null;
	}
	return changes;
};

/** Drops unset values, for a create. */
export const setValues = (values: Record<string, PropertyValue>): Record<string, PropertyValue> =>
	Object.fromEntries(Object.entries(values).filter(([, v]) => !isEmpty(v)));

/** Property errors by name, for a whole form. */
export const validateAll = (
	defs: PropertyDef[],
	values: Record<string, PropertyValue>,
): Record<string, string> => {
	const errors: Record<string, string> = {};
	for (const def of defs) {
		const problem = validateValue(def, values[def.name]);
		if (problem) errors[def.name] = problem;
	}
	return errors;
};

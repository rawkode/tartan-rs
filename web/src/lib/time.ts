/** Times for people: relative for recent moments, absolute in tooltips. */

const units: [Intl.RelativeTimeFormatUnit, number][] = [
	["year", 365 * 86_400_000],
	["month", 30 * 86_400_000],
	["week", 7 * 86_400_000],
	["day", 86_400_000],
	["hour", 3_600_000],
	["minute", 60_000],
];

/** `3 hours ago`, `just now`. */
export const relativeTime = (ms: number, now: number = Date.now()): string => {
	const delta = ms - now;
	const format = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
	for (const [unit, size] of units) {
		if (Math.abs(delta) >= size) return format.format(Math.round(delta / size), unit);
	}
	return "just now";
};

/** The full date and time, for a title attribute. */
export const absoluteTime = (ms: number): string =>
	new Intl.DateTimeFormat(undefined, { dateStyle: "full", timeStyle: "short" }).format(new Date(ms));

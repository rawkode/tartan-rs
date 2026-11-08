/** The forge's slug rule for org and repository names. */

const SLUG = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

/** Why a name is not a valid slug, or `null` when it is. */
export const slugProblem = (text: string): string | null => {
	if (text.length === 0) return "Enter a name.";
	if (text.length > 64) return "At most 64 characters.";
	if (!SLUG.test(text)) return "Use lowercase letters, digits and -, not at either end.";
	return null;
};

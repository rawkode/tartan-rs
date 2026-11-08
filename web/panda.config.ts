import { defineConfig, defineRecipe, defineSlotRecipe } from "@pandacss/dev";
import presetBase from "@pandacss/preset-base";
import presetPanda from "@pandacss/preset-panda";

/** Buttons: one solid call to action per view, outline and ghost for the rest. */
const button = defineRecipe({
	className: "button",
	description: "Clickable actions",
	base: {
		display: "inline-flex",
		alignItems: "center",
		justifyContent: "center",
		gap: "2",
		fontWeight: "medium",
		borderRadius: "md",
		borderWidth: "1px",
		borderColor: "transparent",
		cursor: "pointer",
		whiteSpace: "nowrap",
		transitionProperty: "background, border-color, color, box-shadow",
		transitionDuration: "fast",
		userSelect: "none",
		_focusVisible: { outline: "2px solid", outlineColor: "focus", outlineOffset: "2px" },
		_disabled: { opacity: 0.55, cursor: "not-allowed" },
	},
	variants: {
		variant: {
			solid: {
				bg: "accent.solid",
				color: "accent.contrast",
				_hover: { bg: "accent.emphasis" },
			},
			outline: {
				bg: "bg.surface",
				borderColor: "border.default",
				color: "fg.default",
				_hover: { bg: "bg.subtle", borderColor: "border.strong" },
			},
			ghost: {
				bg: "transparent",
				color: "fg.muted",
				_hover: { bg: "bg.subtle", color: "fg.default" },
			},
			danger: {
				bg: "bg.surface",
				borderColor: "danger.border",
				color: "danger.fg",
				_hover: { bg: "danger.subtle" },
			},
		},
		size: {
			xs: { h: "6", px: "2", fontSize: "xs" },
			sm: { h: "8", px: "3", fontSize: "sm" },
			md: { h: "9", px: "4", fontSize: "sm" },
		},
	},
	defaultVariants: { variant: "outline", size: "md" },
});

/** Text inputs, selects and text areas. */
const field = defineRecipe({
	className: "field",
	description: "Form controls",
	base: {
		width: "100%",
		bg: "bg.surface",
		color: "fg.default",
		borderWidth: "1px",
		borderColor: "border.default",
		borderRadius: "md",
		px: "3",
		fontSize: "sm",
		transitionProperty: "border-color, box-shadow",
		transitionDuration: "fast",
		_placeholder: { color: "fg.subtle" },
		_hover: { borderColor: "border.strong" },
		_focusVisible: {
			outline: "none",
			borderColor: "accent.solid",
			boxShadow: "0 0 0 3px token(colors.accent.ring)",
		},
		_invalid: { borderColor: "danger.border" },
		_disabled: { opacity: 0.6, cursor: "not-allowed" },
	},
	variants: {
		multiline: {
			true: { py: "2", minH: "24", resize: "vertical", lineHeight: "relaxed" },
			false: { h: "9" },
		},
		mono: { true: { fontFamily: "mono", fontSize: "xs" } },
	},
	defaultVariants: { multiline: false },
});

/** Small labels for states, kinds and counts. */
const badge = defineRecipe({
	className: "badge",
	description: "Status and metadata labels",
	base: {
		display: "inline-flex",
		alignItems: "center",
		gap: "1",
		borderRadius: "full",
		px: "2",
		h: "5",
		fontSize: "xs",
		fontWeight: "medium",
		whiteSpace: "nowrap",
		borderWidth: "1px",
	},
	variants: {
		tone: {
			neutral: { bg: "bg.subtle", color: "fg.muted", borderColor: "border.default" },
			accent: { bg: "accent.subtle", color: "accent.fg", borderColor: "accent.border" },
			success: { bg: "success.subtle", color: "success.fg", borderColor: "success.border" },
			danger: { bg: "danger.subtle", color: "danger.fg", borderColor: "danger.border" },
			warning: { bg: "warning.subtle", color: "warning.fg", borderColor: "warning.border" },
		},
	},
	defaultVariants: { tone: "neutral" },
});

/** Bordered surfaces with an optional header and body. */
const card = defineSlotRecipe({
	className: "card",
	description: "Surfaces",
	slots: ["root", "header", "title", "body"],
	base: {
		root: {
			bg: "bg.surface",
			borderWidth: "1px",
			borderColor: "border.default",
			borderRadius: "lg",
			boxShadow: "xs",
			overflow: "hidden",
		},
		header: {
			display: "flex",
			alignItems: "center",
			justifyContent: "space-between",
			gap: "3",
			px: "4",
			py: "3",
			borderBottomWidth: "1px",
			borderColor: "border.default",
			bg: "bg.subtle",
		},
		title: { fontSize: "sm", fontWeight: "semibold", color: "fg.default" },
		body: { p: "4" },
	},
});

export default defineConfig({
	presets: [presetBase, presetPanda],
	preflight: true,
	include: ["./src/**/*.{ts,vue,astro}"],
	exclude: ["./src/**/*.test.ts"],
	outdir: "styled-system",
	jsxFramework: "vue",
	jsxStyleProps: "none",
	strictTokens: false,
	conditions: {
		extend: {
			dark: "[data-theme=dark] &",
			light: "[data-theme=light] &",
		},
	},
	globalCss: {
		"html, body": {
			bg: "bg.canvas",
			color: "fg.default",
			fontFamily: "body",
			fontSize: "sm",
			lineHeight: "normal",
			WebkitFontSmoothing: "antialiased",
			colorScheme: "light",
			_dark: { colorScheme: "dark" },
		},
		"::selection": { bg: "accent.subtle" },
		"code, kbd, pre": { fontFamily: "mono" },
		".sr-only": { srOnly: true },
	},
	theme: {
		extend: {
			tokens: {
				fonts: {
					body: {
						value:
							"'Inter Variable', Inter, ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif",
					},
					mono: {
						value:
							"'JetBrains Mono Variable', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
					},
				},
			},
			semanticTokens: {
				colors: {
					bg: {
						canvas: { value: { base: "{colors.zinc.50}", _dark: "{colors.zinc.950}" } },
						surface: { value: { base: "{colors.white}", _dark: "{colors.zinc.900}" } },
						subtle: { value: { base: "{colors.zinc.100}", _dark: "{colors.zinc.800}" } },
						muted: { value: { base: "{colors.zinc.200}", _dark: "{colors.zinc.700}" } },
						overlay: {
							value: { base: "rgb(24 24 27 / 0.45)", _dark: "rgb(0 0 0 / 0.6)" },
						},
					},
					fg: {
						default: { value: { base: "{colors.zinc.900}", _dark: "{colors.zinc.50}" } },
						muted: { value: { base: "{colors.zinc.600}", _dark: "{colors.zinc.400}" } },
						subtle: { value: { base: "{colors.zinc.500}", _dark: "{colors.zinc.500}" } },
					},
					border: {
						default: { value: { base: "{colors.zinc.200}", _dark: "{colors.zinc.800}" } },
						strong: { value: { base: "{colors.zinc.300}", _dark: "{colors.zinc.600}" } },
					},
					focus: { value: { base: "{colors.emerald.600}", _dark: "{colors.emerald.400}" } },
					accent: {
						solid: { value: { base: "{colors.emerald.700}", _dark: "{colors.emerald.500}" } },
						emphasis: {
							value: { base: "{colors.emerald.800}", _dark: "{colors.emerald.400}" },
						},
						contrast: { value: { base: "{colors.white}", _dark: "{colors.zinc.950}" } },
						fg: { value: { base: "{colors.emerald.800}", _dark: "{colors.emerald.300}" } },
						subtle: {
							value: { base: "{colors.emerald.50}", _dark: "rgb(16 185 129 / 0.14)" },
						},
						border: {
							value: { base: "{colors.emerald.200}", _dark: "rgb(16 185 129 / 0.35)" },
						},
						ring: {
							value: { base: "rgb(4 120 87 / 0.22)", _dark: "rgb(52 211 153 / 0.3)" },
						},
					},
					tartan: {
						red: { value: { base: "{colors.red.700}", _dark: "{colors.red.500}" } },
						green: { value: { base: "{colors.emerald.700}", _dark: "{colors.emerald.500}" } },
						navy: { value: { base: "{colors.blue.900}", _dark: "{colors.blue.400}" } },
					},
					success: {
						fg: { value: { base: "{colors.green.700}", _dark: "{colors.green.300}" } },
						subtle: { value: { base: "{colors.green.50}", _dark: "rgb(34 197 94 / 0.14)" } },
						border: {
							value: { base: "{colors.green.200}", _dark: "rgb(34 197 94 / 0.35)" },
						},
					},
					danger: {
						fg: { value: { base: "{colors.red.700}", _dark: "{colors.red.300}" } },
						subtle: { value: { base: "{colors.red.50}", _dark: "rgb(239 68 68 / 0.14)" } },
						border: { value: { base: "{colors.red.300}", _dark: "rgb(239 68 68 / 0.45)" } },
					},
					warning: {
						fg: { value: { base: "{colors.amber.800}", _dark: "{colors.amber.300}" } },
						subtle: {
							value: { base: "{colors.amber.50}", _dark: "rgb(245 158 11 / 0.14)" },
						},
						border: {
							value: { base: "{colors.amber.300}", _dark: "rgb(245 158 11 / 0.4)" },
						},
					},
					closed: { value: { base: "{colors.violet.700}", _dark: "{colors.violet.300}" } },
				},
			},
			recipes: { button, field, badge },
			slotRecipes: { card },
		},
	},
	staticCss: {
		recipes: {
			button: ["*"],
			field: ["*"],
			badge: ["*"],
			card: ["*"],
		},
	},
});

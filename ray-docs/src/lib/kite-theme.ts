import type { ThemeRegistration } from "shiki/core";

/**
 * Restrained dark theme for marketing surfaces. Graph-related calls read cyan,
 * strings mint, keywords violet — the same accents the homepage uses for
 * traversal, results, and vectors.
 */
export const kiteNight: ThemeRegistration = {
	name: "kite-night",
	type: "dark",
	colors: {
		"editor.background": "#00000000",
		"editor.foreground": "#c8d3e1",
	},
	tokenColors: [
		{
			scope: ["comment", "punctuation.definition.comment"],
			settings: { foreground: "#55657c", fontStyle: "italic" },
		},
		{
			scope: [
				"keyword",
				"storage",
				"storage.type",
				"storage.modifier",
				"keyword.control",
				"keyword.operator.new",
				"keyword.operator.expression",
			],
			settings: { foreground: "#b9a2ff" },
		},
		{
			scope: ["string", "string.quoted", "punctuation.definition.string"],
			settings: { foreground: "#7ee8c4" },
		},
		{
			scope: [
				"entity.name.function",
				"support.function",
				"meta.function-call entity.name.function",
				"variable.function",
			],
			settings: { foreground: "#6fe0ff" },
		},
		{
			scope: [
				"constant.numeric",
				"constant.language",
				"constant.other",
				"support.constant",
			],
			settings: { foreground: "#ffc98a" },
		},
		{
			scope: [
				"entity.name.type",
				"entity.name.class",
				"support.type",
				"support.class",
				"entity.name.namespace",
			],
			settings: { foreground: "#9fc7ff" },
		},
		{
			scope: [
				"variable.other.property",
				"variable.other.object.property",
				"meta.object-literal.key",
				"support.type.property-name",
			],
			settings: { foreground: "#dbe4f0" },
		},
		{
			scope: ["punctuation", "meta.brace", "keyword.operator"],
			settings: { foreground: "#7a879b" },
		},
	],
};

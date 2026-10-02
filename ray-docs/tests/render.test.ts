/**
 * Server-render checks for every page. Requires a running server; see
 * not-found.test.ts.
 */
import { describe, expect, it } from "vitest";
import { docsStructure } from "../src/lib/docs";

const BASE = process.env.DOCS_URL ?? "http://localhost:5311";

// TanStack Start renders this in place of a route whose component throws
// during SSR, still with status 200.
const SSR_ERROR = "Something went wrong";

const pages = [
	"/",
	...docsStructure.flatMap((section) =>
		section.items.map((page) => `/docs/${page.slug}`.replace(/\/$/, "")),
	),
];

describe("server render", () => {
	it.each(pages)("%s renders without an error", async (path) => {
		const response = await fetch(`${BASE}${path}`);
		const html = await response.text();
		expect(response.status).toBe(200);
		expect(html).not.toContain(SSR_ERROR);
	});
});

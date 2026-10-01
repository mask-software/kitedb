/**
 * HTTP-level routing checks. Requires a running server:
 *   bun run dev --port 5311   (or: node .output/server/index.mjs)
 *   DOCS_URL=http://localhost:5311 bun run test
 */
import { describe, expect, it } from "vitest";

const BASE = process.env.DOCS_URL ?? "http://localhost:5311";

const status = async (path: string) => (await fetch(`${BASE}${path}`)).status;

describe("docs routing", () => {
	it("returns 404 for unknown docs pages", async () => {
		const unknown = [
			"/docs/nope",
			"/docs/getting-started/nope",
			"/docs/guides/nope",
			"/docs/api/nope",
			"/docs/benchmarks/nope",
			"/docs/internals/nope",
		];
		for (const path of unknown) {
			expect(await status(path), path).toBe(404);
		}
	});

	it("returns 200 for known docs pages", async () => {
		const known = [
			"/",
			"/docs",
			"/docs/getting-started/installation",
			"/docs/getting-started/quick-start",
			"/docs/guides/schema",
			"/docs/api/high-level",
			"/docs/benchmarks",
			"/docs/benchmarks/graph",
			"/docs/internals/csr",
		];
		for (const path of known) {
			expect(await status(path), path).toBe(200);
		}
	});
});

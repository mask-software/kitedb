import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
	buildModule,
	GENERATED_FILE,
	RESULTS_DIR,
} from "../scripts/benchmark-data";
import { BENCH_LOGS, BENCH_STAMP } from "../src/lib/benchmark-data.gen";
import * as benchmarks from "../src/lib/benchmarks";

describe("published benchmark data", () => {
	it("matches the raw logs it was generated from", () => {
		// Fails when a log or the generated file was edited by hand: rerun
		// `bun run bench:data`.
		expect(readFileSync(GENERATED_FILE, "utf8")).toBe(buildModule(BENCH_STAMP));
	});

	it("names logs that exist", () => {
		const named = new Set<string>(Object.keys(BENCH_LOGS));
		for (const value of Object.values(benchmarks)) {
			const log = (value as { log?: unknown } | null)?.log;
			if (typeof log === "string") named.add(log);
		}
		for (const log of named) {
			expect(existsSync(join(RESULTS_DIR, log)), log).toBe(true);
		}
	});

	it("reads every published row (a renamed row would throw at import)", () => {
		expect(benchmarks.RUST_GRAPH.keyLookup.p50).toBeGreaterThan(0);
		expect(benchmarks.WRITE_SCALING).toHaveLength(4);
		expect(benchmarks.TS_OVERHEAD).toHaveLength(8);
		expect(benchmarks.HEADLINE_STATS.every((s) => s.value !== "NaN")).toBe(
			true,
		);
		expect(benchmarks.BENCH_MACHINE.cpu.length).toBeGreaterThan(0);
	});

	it("shows the day of the run, not its stamp", () => {
		// A second run on one day has a stamp like "2026-10-03-r2".
		const day = new Date(`${BENCH_STAMP.slice(0, 10)}T00:00:00Z`);
		expect(benchmarks.BENCH_DATE).toBe(
			day.toLocaleDateString("en-US", {
				month: "long",
				day: "numeric",
				year: "numeric",
				timeZone: "UTC",
			}),
		);
	});
});

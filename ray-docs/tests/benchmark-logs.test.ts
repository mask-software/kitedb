import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
	manifestNames,
	parseLatency,
	parseLog,
	parseValue,
	renderModule,
} from "../scripts/benchmark-logs";

const RESULTS_DIR = resolve(
	import.meta.dirname,
	"../../docs/benchmarks/results",
);

function round(
	index: number,
	rounds: number,
	body: string,
	status = 0,
): string {
	return [
		`### run ${index}/${rounds} | start 2026-10-05T10:00:00Z | load 1.0 1.0 1.0`,
		body,
		`### run ${index}/${rounds} | exit ${status} | 3s`,
	].join("\n");
}

describe("benchmark log parsing", () => {
	it("converts the units the benches print", () => {
		expect(parseLatency("83ns")).toBe(83);
		expect(parseLatency("34.08us")).toBe(34_080);
		expect(parseLatency("2.98ms")).toBe(2_980_000);
		expect(parseLatency("1.842s")).toBe(1_842_000_000);
		expect(parseValue("173.69", "K/s")).toBe(173_690);
		expect(parseValue("1.26", "M/s")).toBe(1_260_000);
		expect(parseValue("268,435,456", "bytes")).toBe(268_435_456);
	});

	it("reads a 2026-02-04 log, which has no rounds, as published", () => {
		const log = "2026-02-04-single-file-raw-rust-edges-normal-nogc.txt";
		const parsed = parseLog(log, readFileSync(join(RESULTS_DIR, log), "utf8"));
		expect(parsed.rounds).toBe(1);
		expect(parsed.header["Sync mode"]).toBe("Normal");
		expect(parsed.rows["Random existing keys"].fields).toMatchObject({
			p50: 125,
			p95: 291,
		});
		expect(parsed.rows["Batch of 100 nodes"].fields).toMatchObject({
			p50: 34_080,
			p95: 56_540,
		});
		expect(parsed.rows["Batch of 100 edges + props"].line).toBe(
			"Batch of 100 edges + props                    p50=  172.33us p95=  253.12us p99=  420.67us max=  420.67us (5459 ops/sec)",
		);
	});

	it("publishes, per row, the whole line of the round with the median primary value", () => {
		const text = [
			"# name: demo",
			"# commit: abc123 (abc123)",
			"#",
			round(1, 3, "Random nodes   p50=  300ns p95=  900ns\nTx rate: 3.10K/s"),
			round(2, 3, "Random nodes   p50=  100ns p95=  500ns\nTx rate: 3.30K/s"),
			round(3, 3, "Random nodes   p50=  200ns p95=  400ns\nTx rate: 3.20K/s"),
		].join("\n");
		const parsed = parseLog("demo.txt", text);
		expect(parsed.meta).toEqual({ name: "demo", commit: "abc123 (abc123)" });
		expect(parsed.rounds).toBe(3);
		// p95 comes from the median-p50 round, not from a median of p95s
		expect(parsed.rows["Random nodes"]).toMatchObject({
			round: 3,
			primary: "p50",
			fields: { p50: 200, p95: 400 },
		});
		expect(parsed.rows["Tx rate"]).toMatchObject({
			round: 3,
			fields: { value: 3_200 },
		});
	});

	it("takes the lower middle round of an even count", () => {
		const text = [1, 4, 2, 3]
			.map((v, i) =>
				round(i + 1, 4, `Batch of 100 nodes   p50= ${v}0.00us p95= 90.00us`),
			)
			.join("\n");
		expect(parseLog("even.txt", text).rows["Batch of 100 nodes"]).toMatchObject(
			{
				round: 3,
				fields: { p50: 20_000 },
			},
		);
	});

	it("refuses a failed or cut-short round, and rounds with different rows", () => {
		expect(() =>
			parseLog("failed.txt", round(1, 1, "Tx rate: 1.00K/s", 101)),
		).toThrow(/exited with status 101/);
		expect(() =>
			parseLog(
				"cut.txt",
				"### run 1/1 | start x | load 1 1 1\nTx rate: 1.00K/s",
			),
		).toThrow(/no end line/);
		expect(() =>
			parseLog(
				"rows.txt",
				[
					round(1, 2, "Tx rate: 1.00K/s"),
					round(2, 2, "Node rate: 1.00K/s"),
				].join("\n"),
			),
		).toThrow(/different rows/);
	});

	it("prefixes the section to a name a round repeats", () => {
		const body = [
			"Insert Operations:",
			"  Low-level:  p50=    7.71us  p95=   12.00us  (1 ops/sec)",
			"Key Lookups:",
			"  Low-level:  p50=     208ns  p95=     333ns  (1 ops/sec)",
			"Insert (single node + props)             low-level p50=    7.71us  fluent p50=    8.63us  overhead=1.12x",
		].join("\n");
		const parsed = parseLog("ts.txt", body);
		expect(parsed.rows["Insert Operations › Low-level"].fields.p50).toBe(7_710);
		expect(parsed.rows["Key Lookups › Low-level"].fields.p50).toBe(208);
		expect(parsed.rows["Insert (single node + props)"]).toMatchObject({
			primary: "overhead",
			fields: { lowLevel: 7_710, fluent: 8_630, overhead: 1.12 },
		});
	});

	it("reads query_core_bench, bulk_load_bench and mvcc_overhead_bench rows", () => {
		const body = [
			"MVCC: on",
			"paging: 1000000 nodes, 5000000 edges, page size 100, pages 1 and 1000",
			"  snapshot nodes page 1000                              median          0.8 us   min          0.7 us",
			"run 1: nodes   1575796/s (  126.9 ms)  edges    602341/s ( 1660.2 ms)",
			"section op            threads metric              off           on   vs off",
			"reads   node_prop           1 reads/s        48560415     47617562    -1.9%",
			"writes  insert             1w commits/s        459881       369158   -19.7%",
		].join("\n");
		const parsed = parseLog("extras.txt", body);
		expect(parsed.header.MVCC).toBe("on");
		expect(parsed.rows["snapshot nodes page 1000"].fields).toEqual({
			median: 800,
			min: 700,
		});
		expect(parsed.rows["Node rate"].fields).toEqual({
			value: 1_575_796,
			ms: 126.9,
		});
		expect(parsed.rows["Edge rate"].fields).toEqual({
			value: 602_341,
			ms: 1660.2,
		});
		expect(parsed.rows["reads node_prop 1 reads/s"].fields).toEqual({
			off: 48_560_415,
			on: 47_617_562,
			"vs off": -1.9,
		});
		expect(parsed.rows["writes insert 1w commits/s"].fields["vs off"]).toBe(
			-19.7,
		);
	});

	it("lists a manifest's configurations and renders a module", () => {
		const manifest = [
			"# KiteDB benchmark refresh",
			"#",
			"# configurations (log: 2026-10-05-<name>.txt):",
			"#   single-file-raw-rust-mvcc-normal [site, rounds 5]: cargo run ...",
			"#   vector-bench-rust [site, rounds 5]: cargo run ...",
			"#",
			"# runs (round name exit elapsed):",
		].join("\n");
		expect(manifestNames(manifest)).toEqual([
			"single-file-raw-rust-mvcc-normal",
			"vector-bench-rust",
		]);
		const source = renderModule("2026-10-05", [
			parseLog("x.txt", "Tx rate: 1.00K/s"),
		]);
		expect(source).toContain('export const BENCH_STAMP = "2026-10-05";');
		expect(source).toContain('"x.txt": {');
		expect(source.trimEnd().endsWith("} as const;")).toBe(true);
	});
});

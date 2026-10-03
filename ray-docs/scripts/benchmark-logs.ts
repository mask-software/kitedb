/**
 * Parses the raw benchmark logs in docs/benchmarks/results/ into the rows the
 * site publishes. Pure functions; scripts/benchmark-data.ts is the CLI.
 *
 * A log written by ray-rs/scripts/bench-refresh.sh starts with "# key: value"
 * header lines and holds one or more rounds, each between
 * "### run i/N | start ..." and "### run i/N | exit S | ..." lines. A log
 * without run markers is one round.
 *
 * Each result line of a round becomes a row, keyed by its name:
 * - latency rows:    "<name>  p50=83ns p95=125ns p99=... max=..."
 * - TS comparisons:  "<name>  low-level p50=208ns  fluent p50=1.71us  overhead=8.21x"
 * - query_core rows: "  snapshot <name>  median 0.8 us   min 0.7 us" (key "snapshot <name>")
 * - bulk_load runs:  "run 1: nodes 1500000/s (133.3 ms)  edges ..." (keys "Node rate", "Edge rate")
 * - mvcc_overhead:   "<section> <op> <threads> <metric>  <one value per mode column>"
 * - numeric values:  "<name>: <number><unit>" ("Tx rate: 868.44/s", "build_index(): 801.95ms")
 * A name that occurs twice in a round is prefixed with its section ("--- X ---"
 * or a "Heading:" line): "Key Lookups › Low-level".
 *
 * A published row is the round with the median primary value of that row (p50,
 * overhead, median, rate), copied whole: every number is a line of the log.
 * With an even number of rounds it is the lower middle round.
 *
 * Units: latencies and durations in nanoseconds, rates per second.
 */

/** One result line of one round. */
export interface BenchRow {
	/** 1-based round the line comes from */
	round: number;
	/** Field the median round was chosen by */
	primary: string;
	/** Numeric fields: latencies in ns, rates per second */
	fields: Record<string, number>;
	/** The line itself, trimmed */
	line: string;
}

export interface ParsedLog {
	/** File name in docs/benchmarks/results/ */
	log: string;
	/** "# key: value" lines written by bench-refresh.sh (empty for older logs) */
	meta: Record<string, string>;
	/** "Key: value" lines the bench printed in its first round */
	header: Record<string, string>;
	/** Rounds in the log */
	rounds: number;
	/** Published rows by name */
	rows: Record<string, BenchRow>;
}

interface RoundRow {
	section: string;
	name: string;
	primary: string;
	fields: Record<string, number>;
	line: string;
}

const LATENCY_UNITS: Record<string, number> = {
	ns: 1,
	us: 1_000,
	µs: 1_000,
	ms: 1_000_000,
	s: 1_000_000_000,
};

/** "83ns", "34.08us", "2.98ms", "1.842s" -> nanoseconds. */
export function parseLatency(text: string): number {
	const match = /^(-?[\d.]+)\s*(ns|us|µs|ms|s)$/.exec(text.trim());
	if (!match) throw new Error(`not a latency: ${text}`);
	return roundTo(Number(match[1]) * LATENCY_UNITS[match[2]], 3);
}

/** A value with an optional unit: latencies to ns, "K/s" and "M/s" rates to per second. */
export function parseValue(number: string, unit: string): number {
	const value = Number(number.replaceAll(",", ""));
	if (!Number.isFinite(value)) throw new Error(`not a number: ${number}`);
	if (unit in LATENCY_UNITS) return roundTo(value * LATENCY_UNITS[unit], 3);
	const rate = /^([KM]?)\/s$/.exec(unit);
	if (rate) {
		const scale = rate[1] === "M" ? 1_000_000 : rate[1] === "K" ? 1_000 : 1;
		return roundTo(value * scale, 3);
	}
	return value;
}

/** Rounds away float noise from unit conversion (34.08 * 1000 = 34080.000000000004). */
function roundTo(value: number, digits: number): number {
	const scale = 10 ** digits;
	return Math.round(value * scale) / scale;
}

const LATENCY_ROW =
	/^(?<name>.*?)\s+p50=\s*(?<p50>\S+)\s+p95=\s*(?<p95>\S+)(?:\s+p99=\s*(?<p99>\S+))?(?:\s+max=\s*(?<max>\S+))?/;
const COMPARISON_ROW =
	/^(?<name>.+?)\s+low-level p50=\s*(?<low>\S+)\s+fluent p50=\s*(?<fluent>\S+)\s+overhead=(?<overhead>[\d.]+)x/;
const QUERY_CORE_ROW =
	/^(?<state>delta|snapshot)\s+(?<name>.+?)\s+median\s+(?<median>[\d.]+) us\s+min\s+(?<min>[\d.]+) us$/;
const BULK_RUN =
	/^run \d+: nodes\s+(?<nodes>[\d.]+)\/s \(\s*(?<nodesMs>[\d.]+) ms\)\s+edges\s+(?<edges>[\d.]+)\/s \(\s*(?<edgesMs>[\d.]+) ms\)$/;
const MVCC_TABLE_HEADER = /^section\s+op\s+threads\s+metric\s+(?<columns>.+)$/;
const MVCC_TABLE_ROW =
	/^(?<section>reads|mixed|writes)\s+(?<op>\S+)\s+(?<threads>\S+)\s+(?<metric>\S+)\s+(?<values>.+)$/;
const NUMERIC_LINE =
	/^(?<name>[A-Za-z_][\w ()./-]*?):\s+(?<number>-?[\d][\d.,]*)\s*(?<unit>[A-Za-zµ/%]*)$/;
const TEXT_LINE = /^(?<name>[A-Za-z_][\w ()./-]*?):\s+(?<value>\S.*)$/;
const SECTION_DASHES = /^-{3}\s+(?<name>.+?)\s+-{3}$/;
const SECTION_HEADING = /^(?<name>[A-Z][^:=]*):$/;
const META_LINE = /^#\s+(?<key>[^:]+):\s+(?<value>.*)$/;
const RUN_START = /^### run (?<round>\d+)\/(?<rounds>\d+) \| start/;
const RUN_END =
	/^### run (?<round>\d+)\/(?<rounds>\d+) \| exit (?<status>-?\d+)/;

/** Named groups of `pattern` in `line`, or undefined. */
function match(
	pattern: RegExp,
	line: string,
): Record<string, string> | undefined {
	return pattern.exec(line)?.groups;
}

/** "off on vs off" -> ["off", "on", "vs off"] */
function mvccColumnNames(columns: string): string[] {
	const names: string[] = [];
	const words = columns.split(/\s+/);
	for (let i = 0; i < words.length; i++) {
		if (words[i] === "vs" && i + 1 < words.length) {
			names.push(`vs ${words[i + 1]}`);
			i++;
		} else {
			names.push(words[i]);
		}
	}
	return names;
}

/** The rows one result line holds (most hold one), or null for other lines. */
function lineRows(
	line: string,
	mvccColumns: string[] | null,
	log: string,
): Omit<RoundRow, "section" | "line">[] | null {
	let g = match(COMPARISON_ROW, line);
	if (g) {
		return [
			{
				name: g.name.trim(),
				primary: "overhead",
				fields: {
					lowLevel: parseLatency(g.low),
					fluent: parseLatency(g.fluent),
					overhead: Number(g.overhead),
				},
			},
		];
	}
	g = match(LATENCY_ROW, line);
	if (g) {
		const fields: Record<string, number> = {
			p50: parseLatency(g.p50),
			p95: parseLatency(g.p95),
		};
		if (g.p99) fields.p99 = parseLatency(g.p99);
		if (g.max) fields.max = parseLatency(g.max);
		return [{ name: g.name.trim().replace(/:$/, ""), primary: "p50", fields }];
	}
	g = match(QUERY_CORE_ROW, line);
	if (g) {
		return [
			{
				name: `${g.state} ${g.name.trim()}`,
				primary: "median",
				fields: {
					median: parseValue(g.median, "us"),
					min: parseValue(g.min, "us"),
				},
			},
		];
	}
	g = match(BULK_RUN, line);
	if (g) {
		return [
			{
				name: "Node rate",
				primary: "value",
				fields: { value: Number(g.nodes), ms: Number(g.nodesMs) },
			},
			{
				name: "Edge rate",
				primary: "value",
				fields: { value: Number(g.edges), ms: Number(g.edgesMs) },
			},
		];
	}
	g = mvccColumns ? match(MVCC_TABLE_ROW, line) : undefined;
	if (g && mvccColumns) {
		const values = g.values.split(/\s+/);
		if (values.length !== mvccColumns.length) {
			throw new Error(
				`${log}: ${values.length} values for ${mvccColumns.length} columns: ${line}`,
			);
		}
		const fields: Record<string, number> = {};
		mvccColumns.forEach((column, i) => {
			if (values[i] !== "-") {
				fields[column] = Number(values[i].replace(/%$/, ""));
			}
		});
		return [
			{
				name: `${g.section} ${g.op} ${g.threads} ${g.metric}`,
				primary: mvccColumns[0],
				fields,
			},
		];
	}
	g = match(NUMERIC_LINE, line);
	if (g) {
		return [
			{
				name: g.name.trim(),
				primary: "value",
				fields: { value: parseValue(g.number, g.unit) },
			},
		];
	}
	return null;
}

/** Rows and "Key: value" lines of one round's output. */
function parseRound(
	lines: string[],
	log: string,
): { rows: RoundRow[]; header: Record<string, string> } {
	const rows: RoundRow[] = [];
	const header: Record<string, string> = {};
	let section = "";
	let mvccColumns: string[] | null = null;

	for (const raw of lines) {
		// Progress lines rewrite themselves with \r; keep what was printed last.
		const line = (raw.split("\r").pop() ?? "").trim();
		if (line === "") continue;

		const dashes = match(SECTION_DASHES, line);
		if (dashes) {
			section = dashes.name;
			continue;
		}
		const table = match(MVCC_TABLE_HEADER, line);
		if (table) {
			mvccColumns = mvccColumnNames(table.columns);
			continue;
		}
		const found = lineRows(line, mvccColumns, log);
		if (found) {
			for (const row of found) rows.push({ ...row, section, line });
			if (!NUMERIC_LINE.test(line)) continue;
		}
		// "Key: value" lines, numeric or not, describe the run.
		const text = match(TEXT_LINE, line);
		if (text) {
			const name = text.name.trim();
			if (!(name in header)) header[name] = text.value.trim();
			continue;
		}
		const heading = match(SECTION_HEADING, line);
		if (heading) section = heading.name;
	}
	return { rows, header };
}

/** Keys rows by name, prefixing the section to names a round repeats. */
function keyRows(rows: RoundRow[], log: string): Map<string, RoundRow> {
	const counts = new Map<string, number>();
	for (const row of rows) counts.set(row.name, (counts.get(row.name) ?? 0) + 1);
	const keyed = new Map<string, RoundRow>();
	for (const row of rows) {
		const key =
			(counts.get(row.name) ?? 0) > 1 && row.section
				? `${row.section} › ${row.name}`
				: row.name;
		if (keyed.has(key)) {
			throw new Error(`${log}: row "${key}" occurs twice in one round`);
		}
		keyed.set(key, row);
	}
	return keyed;
}

/** Splits a log into its header and rounds; a failed round is an error. */
function splitRounds(
	text: string,
	log: string,
): { meta: Record<string, string>; rounds: string[][] } {
	const meta: Record<string, string> = {};
	const rounds: string[][] = [];
	let current: string[] | null = null;
	let sawMarkers = false;
	const loose: string[] = [];

	for (const line of text.split("\n")) {
		if (RUN_START.test(line)) {
			sawMarkers = true;
			current = [];
			continue;
		}
		const runEnd = match(RUN_END, line);
		if (runEnd) {
			if (runEnd.status !== "0") {
				throw new Error(
					`${log}: round ${runEnd.round} exited with status ${runEnd.status}`,
				);
			}
			if (current) rounds.push(current);
			current = null;
			continue;
		}
		const metaLine = current ? undefined : match(META_LINE, line);
		if (current) {
			current.push(line);
		} else if (metaLine) {
			if (!(metaLine.key in meta)) meta[metaLine.key] = metaLine.value;
		} else if (!line.startsWith("#")) {
			loose.push(line);
		}
	}
	if (current)
		throw new Error(`${log}: the last round has no end line (cut short?)`);
	if (!sawMarkers) rounds.push(loose);
	if (rounds.length === 0) throw new Error(`${log}: no rounds`);
	return { meta, rounds };
}

/** Parses one log; `log` is its file name, used in errors and the result. */
export function parseLog(log: string, text: string): ParsedLog {
	const { meta, rounds } = splitRounds(text, log);
	const parsed = rounds.map((lines) => parseRound(lines, log));
	const keyed = parsed.map(({ rows }) => keyRows(rows, log));

	const names = [...keyed[0].keys()];
	for (const [index, round] of keyed.entries()) {
		const missing = names.filter((name) => !round.has(name));
		const extra = [...round.keys()].filter((name) => !keyed[0].has(name));
		if (missing.length > 0 || extra.length > 0) {
			throw new Error(
				`${log}: round ${index + 1} has different rows (missing: ${missing.join(", ") || "none"}; extra: ${extra.join(", ") || "none"})`,
			);
		}
	}

	const rows: Record<string, BenchRow> = {};
	for (const name of names) {
		const candidates = keyed.map((round, index) => {
			const row = round.get(name);
			if (!row) throw new Error(`${log}: round ${index + 1} lacks ${name}`);
			return { round: index + 1, row };
		});
		const primary = candidates[0].row.primary;
		candidates.sort(
			(a, b) =>
				(a.row.fields[primary] ?? 0) - (b.row.fields[primary] ?? 0) ||
				a.round - b.round,
		);
		const median = candidates[Math.floor((candidates.length - 1) / 2)];
		rows[name] = {
			round: median.round,
			primary,
			fields: median.row.fields,
			line: median.row.line,
		};
	}

	return { log, meta, header: parsed[0].header, rounds: rounds.length, rows };
}

/** The configuration names a bench-refresh manifest lists, in order. */
export function manifestNames(text: string): string[] {
	const names: string[] = [];
	let inList = false;
	for (const line of text.split("\n")) {
		if (line.startsWith("# configurations")) {
			inList = true;
			continue;
		}
		if (!inList) continue;
		const entry = match(/^#\s{3}(?<name>[\w.-]+) \[/, line);
		if (!entry) break;
		names.push(entry.name);
	}
	return names;
}

/** The source of src/lib/benchmark-data.gen.ts for parsed logs of one refresh. */
export function renderModule(stamp: string, logs: ParsedLog[]): string {
	const byName: Record<string, Omit<ParsedLog, "log">> = {};
	for (const { log, ...rest } of logs) byName[log] = rest;
	return [
		"// Generated by `bun run bench:data` (ray-docs/scripts/benchmark-data.ts) from",
		`// docs/benchmarks/results/${stamp}-*.txt. Do not edit; rerun the script.`,
		"// Each row is the line of the round with the median value; see",
		"// scripts/benchmark-logs.ts for the rules. Latencies in ns, rates per second.",
		"",
		`export const BENCH_STAMP = ${JSON.stringify(stamp)};`,
		"",
		`export const BENCH_LOGS = ${JSON.stringify(byName, null, "\t")} as const;`,
		"",
	].join("\n");
}

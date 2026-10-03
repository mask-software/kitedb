/**
 * Writes src/lib/benchmark-data.gen.ts from the raw logs of one benchmark
 * refresh (ray-rs/scripts/bench-refresh.sh), so no number on the site is typed
 * by hand.
 *
 * Usage (from ray-docs/):
 *   bun run bench:data [STAMP]     Generate from docs/benchmarks/results/<STAMP>-*.txt,
 *                                  the logs its <STAMP>-bench-refresh.txt manifest lists
 *                                  (default STAMP: the newest manifest)
 *   bun run bench:data --check     Exit 1 if the generated file is out of date
 *   bun run bench:data --results DIR STAMP
 *                                  Read the logs from DIR (e.g. a --smoke run) and
 *                                  print the module instead of writing it
 *   bun run bench:data --print LOG...
 *                                  Print the parsed rows of any logs as JSON
 */

import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { manifestNames, parseLog, renderModule } from "./benchmark-logs";

const DOCS_DIR = resolve(import.meta.dirname, "..");
export const RESULTS_DIR = resolve(DOCS_DIR, "../docs/benchmarks/results");
export const GENERATED_FILE = join(DOCS_DIR, "src/lib/benchmark-data.gen.ts");
const MANIFEST_SUFFIX = "-bench-refresh.txt";

/** STAMP of the newest bench-refresh manifest in the results directory. */
export function latestStamp(resultsDir = RESULTS_DIR): string {
	const stamps = readdirSync(resultsDir)
		.filter((file) => file.endsWith(MANIFEST_SUFFIX))
		.map((file) => file.slice(0, -MANIFEST_SUFFIX.length))
		.sort();
	const stamp = stamps.at(-1);
	if (!stamp)
		throw new Error(`no *${MANIFEST_SUFFIX} manifest in ${resultsDir}`);
	return stamp;
}

/** The generated module for one refresh. */
export function buildModule(stamp: string, resultsDir = RESULTS_DIR): string {
	const manifest = join(resultsDir, `${stamp}${MANIFEST_SUFFIX}`);
	const names = manifestNames(readFileSync(manifest, "utf8"));
	if (names.length === 0)
		throw new Error(`${manifest} lists no configurations`);
	const logs = names.map((name) => {
		const log = `${stamp}-${name}.txt`;
		return parseLog(log, readFileSync(join(resultsDir, log), "utf8"));
	});
	return renderModule(stamp, logs);
}

function main(args: string[]): number {
	if (args[0] === "--print") {
		const parsed = args
			.slice(1)
			.map((path) => parseLog(basename(path), readFileSync(path, "utf8")));
		console.log(JSON.stringify(parsed, null, 2));
		return 0;
	}
	const check = args.includes("--check");
	const resultsAt = args.indexOf("--results");
	const resultsDir = resultsAt >= 0 ? args[resultsAt + 1] : RESULTS_DIR;
	if (!resultsDir) throw new Error("--results needs a directory");
	const positional = args.filter(
		(arg, i) => !arg.startsWith("--") && i !== resultsAt + 1,
	);
	const stamp = positional[0] ?? latestStamp(resultsDir);
	const source = buildModule(stamp, resultsDir);
	if (resultsAt >= 0) {
		process.stdout.write(source);
		return 0;
	}
	if (check) {
		const current = existsSync(GENERATED_FILE)
			? readFileSync(GENERATED_FILE, "utf8")
			: "";
		if (current !== source) {
			console.error(
				`${GENERATED_FILE} is out of date: run bun run bench:data ${stamp}`,
			);
			return 1;
		}
		console.log(`${GENERATED_FILE} matches the ${stamp} logs`);
		return 0;
	}
	writeFileSync(GENERATED_FILE, source);
	console.log(`wrote ${GENERATED_FILE} from the ${stamp} logs`);
	return 0;
}

if (import.meta.main) {
	process.exit(main(process.argv.slice(2)));
}

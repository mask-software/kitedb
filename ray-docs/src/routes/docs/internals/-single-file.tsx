import { For, type JSX, Show } from "solid-js";
import DocPage from "~/components/doc-page";
import {
	Code,
	Figure,
	FlowItem,
	Steps,
	type Accent,
	type Step,
} from "./-components";

// ============================================================================
// SHARED DIAGRAM PIECES
// ============================================================================

interface Field {
	name: string;
	value: string;
	mono?: boolean;
	detail?: string;
}

/** Two-column field list used for the header and snapshot sections. */
function FieldList(props: { fields: Field[] }) {
	return (
		<dl class="divide-y divide-kite-line/70 text-[14px]">
			<For each={props.fields}>
				{(field) => (
					<div class="grid gap-x-4 gap-y-0.5 py-2.5 first:pt-0 last:pb-0 sm:grid-cols-[11rem_1fr]">
						<dt class="text-slate-200">{field.name}</dt>
						<dd class="text-slate-400">
							<span class={field.mono ? "font-mono text-[13px]" : undefined}>
								{field.value}
							</span>
							<Show when={field.detail}>
								<span class="ml-2 text-[13px] text-slate-500">
									{field.detail}
								</span>
							</Show>
						</dd>
					</div>
				)}
			</For>
		</dl>
	);
}

/** Short note describing how v0.2.18 and earlier behaved. */
function VersionNote(props: { children: JSX.Element }) {
	return (
		<div class="my-6 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3 text-[14px] text-slate-400">
			<span class="text-slate-200">v0.2.18 and earlier:</span> {props.children}
		</div>
	);
}

// ============================================================================
// SINGLE-FILE DIAGRAMS
// ============================================================================

const REGION_BAR: Record<Accent, string> = {
	red: "bg-red-400/60",
	cyan: "bg-kite-cyan/60",
	violet: "bg-kite-violet/60",
	mint: "bg-kite-mint/60",
	amber: "bg-amber-400/60",
	slate: "bg-slate-500/60",
};

function FileRegion(props: {
	name: string;
	size: string;
	accent: Accent;
	children: JSX.Element;
}) {
	return (
		<div class="relative px-4 py-3.5">
			<span
				class={`absolute top-0 bottom-0 left-0 w-0.5 ${REGION_BAR[props.accent]}`}
				aria-hidden="true"
			/>
			<div class="flex items-baseline justify-between gap-4">
				<span class="text-[15px] font-semibold text-white">{props.name}</span>
				<span class="shrink-0 font-mono text-[12px] text-slate-500">
					{props.size}
				</span>
			</div>
			<div class="mt-1 text-[14px] text-slate-400">{props.children}</div>
		</div>
	);
}

function FileLayoutDiagram() {
	return (
		<Figure title="Regions of a .kitedb file" accent="cyan">
			<div class="divide-y divide-kite-line overflow-hidden rounded-lg border border-kite-line bg-white/[0.02]">
				<FileRegion name="Header" size="pages 0–1" accent="cyan">
					Two checksummed copies of the database metadata and region pointers
					<div class="mt-3 flex overflow-hidden rounded-md border border-kite-line font-mono text-[12px]">
						<div class="min-w-0 flex-1 border-r border-kite-line px-3 py-2">
							<div class="text-slate-200">Page 0</div>
							<div class="text-[11px] text-slate-500">header copy A</div>
						</div>
						<div class="min-w-0 flex-1 px-3 py-2">
							<div class="text-slate-200">Page 1</div>
							<div class="text-[11px] text-slate-500">header copy B</div>
						</div>
					</div>
				</FileRegion>
				<FileRegion
					name="WAL area"
					size="from page 2, 4 MB default"
					accent="mint"
				>
					Write-ahead log, split into two regions
					<div class="mt-3 flex overflow-hidden rounded-md border border-kite-line font-mono text-[12px]">
						<div class="min-w-0 flex-1 border-r border-kite-line bg-kite-mint/[0.07] px-3 py-2">
							<div class="text-slate-200">Primary</div>
							<div class="text-[11px] text-slate-500">75%, normal writes</div>
						</div>
						<div class="w-1/4 min-w-[6.5rem] px-3 py-2">
							<div class="text-slate-200">Secondary</div>
							<div class="text-[11px] text-slate-500">
								25%, during checkpoint
							</div>
						</div>
					</div>
				</FileRegion>
				<FileRegion name="Snapshot area" size="grows" accent="violet">
					CSR graph data, compressed with zstd. Each checkpoint writes a new
					snapshot to a free region.
				</FileRegion>
			</div>
		</Figure>
	);
}

function HeaderContents() {
	const fields: Field[] = [
		{
			name: "Magic bytes",
			value: '"KiteDB format 1\\0"',
			mono: true,
			detail: "16 bytes",
		},
		{ name: "Versions", value: "Format version, minimum reader version" },
		{ name: "Page size", value: "4096", mono: true, detail: "default" },
		{
			name: "Change counter",
			value: "Incremented on every header write; open uses the higher one",
		},
		{ name: "Snapshot location", value: "Start page, page count" },
		{ name: "Snapshot generation", value: "Incremented on each checkpoint" },
		{ name: "WAL location", value: "Start page, page count" },
		{
			name: "WAL pointers",
			value: "Head and tail, plus each WAL region's head and the active region",
		},
		{
			name: "Checkpoint flag",
			value: "Set while a background checkpoint is running",
		},
		{
			name: "Counters",
			value: "Max node ID, next transaction ID, last commit timestamp",
		},
		{
			name: "Checksums",
			value: "CRC32C over the header fields and over the whole page",
		},
	];
	return (
		<Figure title="Header fields" accent="cyan" meta="pages 0 and 1">
			<FieldList fields={fields} />
		</Figure>
	);
}

function AtomicCheckpointProcess() {
	const steps: Step[] = [
		{
			text: "Build the new snapshot in memory from the current snapshot and the delta",
			accent: "violet",
		},
		{
			text: "Write it to a free region",
			sub: "A retired snapshot region if the new snapshot fits, otherwise the end of the file. The current snapshot's pages are never overwritten.",
			accent: "violet",
		},
		{
			text: (
				<>
					<Code>fsync()</Code> so the snapshot is durable
				</>
			),
			accent: "violet",
		},
		{
			text: "Write a header that points to the new snapshot into the inactive header page",
			accent: "violet",
		},
		{
			text: (
				<>
					<Code>fsync()</Code> the header
				</>
			),
			accent: "mint",
			note: "new snapshot is current",
		},
		{
			text: (
				<>
					Write the same header into the other page and <Code>fsync()</Code>
				</>
			),
			sub: "Now neither header page points to the old snapshot",
			accent: "slate",
		},
		{
			text: "Retire the old snapshot's region so a later checkpoint can reuse it",
			accent: "slate",
		},
	];
	return (
		<Figure title="Checkpoint sequence" accent="violet">
			<Steps steps={steps} />
			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="mb-2 text-[13px] text-slate-500">If the process crashes:</p>
				<div class="grid gap-2 text-[13px] sm:grid-cols-2">
					<div class="rounded-md border border-amber-400/25 bg-amber-400/[0.05] px-3 py-2">
						<span class="font-medium text-amber-300">Before step 5:</span>{" "}
						<span class="text-slate-300">
							open uses the previous header, which still points to the old
							snapshot and the WAL records it needs
						</span>
					</div>
					<div class="rounded-md border border-kite-mint/25 bg-kite-mint/[0.05] px-3 py-2">
						<span class="font-medium text-kite-mint">After step 5:</span>{" "}
						<span class="text-slate-300">
							open uses the new header and the new snapshot
						</span>
					</div>
				</div>
				<p class="mt-2 text-[13px] text-slate-500">
					A header write torn by the crash fails its checksum, and open uses the
					other header page.
				</p>
			</div>
		</Figure>
	);
}

function WALDualRegion() {
	return (
		<Figure title="WAL regions" accent="mint" meta="64 MB example">
			<div class="flex overflow-hidden rounded-md border border-kite-line font-mono text-[12px]">
				<div class="min-w-0 flex-1 border-r border-kite-line bg-kite-mint/[0.07] px-3 py-2.5">
					<div class="text-slate-200">Primary</div>
					<div class="text-[11px] text-slate-500">48 MB</div>
				</div>
				<div class="w-1/4 min-w-[6.5rem] px-3 py-2.5">
					<div class="text-slate-200">Secondary</div>
					<div class="text-[11px] text-slate-500">16 MB</div>
				</div>
			</div>
			<p class="mt-4 mb-2 text-[14px] text-slate-300">
				Two regions let a checkpoint run while writes continue:
			</p>
			<div class="space-y-1.5">
				<FlowItem color="violet">
					The checkpoint builds the new snapshot from the in-memory delta, which
					holds the changes logged in the primary region
				</FlowItem>
				<FlowItem color="emerald">
					Transactions that commit in the meantime write to the secondary
					region, so writers don't wait for the snapshot to be built
				</FlowItem>
			</div>
		</Figure>
	);
}

function SnapshotSections() {
	const fields: Field[] = [
		{ name: "Node ID mappings", value: "Physical ↔ logical ID translation" },
		{
			name: "Out-edge CSR",
			value: "offsets[], destinations[], edge_types[]",
			mono: true,
		},
		{
			name: "In-edge CSR",
			value: "offsets[], sources[], edge_types[]",
			mono: true,
		},
		{ name: "Properties", value: "Node and edge property values" },
		{ name: "String table", value: "Deduplicated string storage" },
		{ name: "Key index", value: "Hash-bucketed node key lookups" },
		{ name: "Schema", value: "Labels, edge types, property keys" },
	];
	return (
		<Figure title="Snapshot sections" accent="violet">
			<FieldList fields={fields} />
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-500">
				Each section is compressed independently with zstd. Typical size: 40–60%
				of the raw data.
			</p>
		</Figure>
	);
}

const GROWTH_ROWS = [
	{
		label: "Initial",
		wal: "flex-1",
		snapshot: "w-1 bg-kite-violet/25",
		size: "~64 MB",
	},
	{
		label: "100K nodes",
		wal: "w-3/4",
		snapshot: "w-1/5 bg-kite-violet/45",
		size: "~72 MB",
	},
	{
		label: "1M nodes",
		wal: "w-1/2",
		snapshot: "w-2/5 bg-kite-violet/45",
		size: "~150 MB",
	},
];

function FileGrowthDiagram() {
	return (
		<Figure title="File size examples" accent="cyan" meta="64 MB WAL">
			<p class="mb-4 text-[13px] text-slate-500">
				These examples assume a 64 MB WAL. The default WAL size is 4 MB and is
				configurable.
			</p>
			<div class="space-y-3">
				<For each={GROWTH_ROWS}>
					{(row) => (
						<div class="grid grid-cols-[5.5rem_1fr_4rem] items-center gap-3">
							<span class="text-[13px] text-slate-400">{row.label}</span>
							<div class="flex h-4 items-stretch gap-0.5">
								<div class="w-1 rounded-sm bg-kite-cyan/70" />
								<div class={`rounded-sm bg-kite-mint/25 ${row.wal}`} />
								<div class={`rounded-sm ${row.snapshot}`} />
							</div>
							<span class="text-right font-mono text-[12px] text-slate-200">
								{row.size}
							</span>
						</div>
					)}
				</For>
			</div>
			<div class="mt-4 flex flex-wrap gap-x-5 gap-y-1.5 border-t border-kite-line pt-3 text-[12px] text-slate-500">
				<span class="flex items-center gap-1.5">
					<span class="h-2 w-2 rounded-sm bg-kite-cyan/70" />
					Header (fixed)
				</span>
				<span class="flex items-center gap-1.5">
					<span class="h-2 w-2 rounded-sm bg-kite-mint/25" />
					WAL (configurable)
				</span>
				<span class="flex items-center gap-1.5">
					<span class="h-2 w-2 rounded-sm bg-kite-violet/45" />
					Snapshot (grows)
				</span>
			</div>
		</Figure>
	);
}

function DatabaseOpenProcess() {
	const steps: Step[] = [
		{
			text: "Lock the file",
			sub: "Exclusive for a writable open, shared for a read-only open",
			accent: "cyan",
		},
		{
			text: "Read both header pages and use the newest one that passes its checksums",
			sub: "A writable open converts a file in the old single-header layout first",
			accent: "cyan",
		},
		{
			text: "If a background checkpoint was interrupted, finish or undo its cut",
			sub: "If the secondary region's records fit after the primary's, a writable open appends them; otherwise both regions are replayed in place and the next background checkpoint resumes the cut. Read-only opens replay in place without writing.",
			accent: "amber",
		},
		{
			text: (
				<>
					<Code>mmap()</Code> the snapshot area
				</>
			),
			accent: "violet",
		},
		{ text: "Parse snapshot sections", accent: "violet" },
		{
			text: "Replay committed WAL transactions, in commit order, to rebuild the delta",
			accent: "slate",
		},
		{ text: "Ready for queries", accent: "mint", done: true },
	];
	return (
		<Figure title="Open sequence" accent="cyan">
			<Steps steps={steps} />
			<p class="mt-5 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				<span class="text-amber-300">Incomplete transactions</span> found during
				WAL replay are discarded, since they never committed. Recovery runs
				automatically on open.
			</p>
		</Figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function SingleFilePage() {
	return (
		<DocPage slug="internals/single-file">
			<p>
				KiteDB stores a database in one <code>.kitedb</code> file containing a
				header, a write-ahead log, and a snapshot. One file is easy to copy and
				deploy, and the header records where the current snapshot and WAL are.
			</p>

			<h2 id="file-layout">File layout</h2>

			<FileLayoutDiagram />

			<VersionNote>
				files have one header page (page 0), and the WAL starts at page 1. The
				current version converts such a file on its first writable open: it
				writes the recovered database to a temporary file and renames it over
				the original, so a crash during conversion leaves the original intact.
				Read-only opens use old files without converting them. Don't open a
				converted file with v0.2.18 or earlier, which read and write only page
				0.
			</VersionNote>

			<h2 id="header">The header</h2>

			<p>
				The header holds the metadata needed to open the database. KiteDB keeps
				two copies of it, in pages 0 and 1 (4 KB each at the default page size).
				Each header update is written to the copy that is not current, so the
				current copy is never overwritten. On open, KiteDB checks both copies
				and uses the valid one with the higher change counter. If a crash tears
				a header write, that copy fails its checksum and the other copy is used.
			</p>

			<HeaderContents />

			<VersionNote>
				the single header page was rewritten in place, so a torn header write
				could leave the database unopenable.
			</VersionNote>

			<h2 id="atomicity">Atomic updates</h2>

			<p>
				A checkpoint writes the new snapshot to a different region than the
				current one and then switches the header to it. Blocking and background
				checkpoints follow the same order; the{" "}
				<a href="/docs/internals/wal">WAL page</a> covers how background
				checkpoints handle writes that arrive in the meantime.
			</p>

			<AtomicCheckpointProcess />

			<VersionNote>
				the new snapshot overwrote the previous one, right after the WAL, so a
				crash during a checkpoint could leave the file without a valid snapshot.
			</VersionNote>

			<h2 id="wal-area">WAL area</h2>

			<p>
				The WAL area is a fixed-size, append-only log divided into two regions.
			</p>
			<p>
				The default WAL size is 4 MB. Auto-checkpoint is on by default and runs
				when the active region is 50% full (<code>checkpointThreshold</code>).
				Use a larger WAL for high-throughput ingest. The size is fixed when the
				file is created; change it with <code>resizeWal</code> (offline) or
				rebuild into a new file.
			</p>

			<WALDualRegion />

			<h2 id="snapshot-area">Snapshot area</h2>

			<p>The snapshot area holds the graph data in CSR format:</p>

			<SnapshotSections />

			<h2 id="growth">File growth</h2>

			<p>
				The header and WAL have fixed sizes; the snapshot grows with your data:
			</p>

			<FileGrowthDiagram />

			<p>
				Because a checkpoint never overwrites the current snapshot, the file can
				hold a retired snapshot region next to the current one. A later
				checkpoint reuses a retired region when the new snapshot fits, and
				retired space at the end of the file is truncated. Retired regions are
				tracked in memory, so space that is still retired when the database
				closes stays in the file until you run <code>vacuum</code>.
			</p>

			<h2 id="vs-directory">Single-file vs multi-file</h2>

			<p>
				KiteDB previously supported a directory-based format. Single-file is now
				the default:
			</p>

			<table>
				<thead>
					<tr>
						<th>Aspect</th>
						<th>Single-file</th>
						<th>Directory (legacy)</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Portability</td>
						<td>Copy one file</td>
						<td>Copy entire directory</td>
					</tr>
					<tr>
						<td>Atomic ops</td>
						<td>Header flip</td>
						<td>Manifest + renames</td>
					</tr>
					<tr>
						<td>Disk usage</td>
						<td>~40% smaller</td>
						<td>More overhead</td>
					</tr>
					<tr>
						<td>Complexity</td>
						<td>Simpler</td>
						<td>More moving parts</td>
					</tr>
				</tbody>
			</table>

			<h2 id="opening">Opening a database</h2>

			<DatabaseOpenProcess />

			<p>
				The file lock lets one writer or any number of readers use a file at a
				time. A writable open fails if another open of the same file, in this
				process or another, holds any lock on it. A read-only open fails only if
				a writer holds it.
			</p>
			<p>
				A read-only open asks the operating system for read access only and
				never writes to the file. WAL replay rebuilds the delta in memory, and
				closing the database leaves the file untouched. After an interrupted
				background checkpoint, a read-only open replays both WAL regions in
				place; the next writable open finishes the repair.
			</p>

			<VersionNote>
				opens took no file lock, so two processes could write the same file at
				once. Read-only opens still opened the file for writing and could
				rewrite the header on open and on close. WAL replay applied committed
				transactions in arbitrary order instead of commit order.
			</VersionNote>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/wal">WAL and durability</a>: how the
					write-ahead log provides crash safety
				</li>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a>: how
					reads merge these two sources
				</li>
			</ul>
		</DocPage>
	);
}

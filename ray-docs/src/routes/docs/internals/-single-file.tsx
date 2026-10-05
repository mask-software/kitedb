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
					Write-ahead log of fixed size. Every record is written to the primary
					region; the secondary region is only read, in files from an earlier
					version
					<div class="mt-3 flex overflow-hidden rounded-md border border-kite-line font-mono text-[12px]">
						<div class="min-w-0 flex-1 border-r border-kite-line bg-kite-mint/[0.07] px-3 py-2">
							<div class="text-slate-200">Primary</div>
							<div class="text-[11px] text-slate-500">75%, all writes</div>
						</div>
						<div class="w-1/4 min-w-[6.5rem] px-3 py-2">
							<div class="text-slate-200">Secondary</div>
							<div class="text-[11px] text-slate-500">25%, not written</div>
						</div>
					</div>
				</FileRegion>
				<FileRegion name="Snapshot area" size="grows" accent="violet">
					CSR graph data, compressed with zstd. Each checkpoint writes a new
					snapshot to pages no header names.
				</FileRegion>
				<FileRegion name="WAL segments" size="as needed" accent="amber">
					Extents of pages holding the records the WAL spilled when it filled,
					anywhere after the WAL area. The header's segment table names them,
					and a checkpoint frees the ones it covers.
				</FileRegion>
			</div>
		</Figure>
	);
}

function HeaderContents() {
	const fields: Field[] = [
		{
			name: "Magic bytes",
			value: '"KiteDB format 2\\0"',
			mono: true,
			detail:
				'16 bytes. Releases v0.2.3 to v0.2.18 accept only "KiteDB format 1\\0" (earlier ones only "RayDB format 1"), so they refuse these files instead of misreading them. This version still reads that magic, and a writable open rewrites both slots in the new one as its last step, so an open that fails leaves them in the old one; a crash between the two writes can leave page 0 in the old magic, which v0.2.18 then opens as the file was before that open, until the next writable open finishes. It refuses files of v0.1.4 to v0.2.2 ("RayDB format 1"), as releases have since v0.2.3',
		},
		{
			name: "Versions",
			value:
				"Format version, minimum reader version, and feature flags. A header that names WAL segments is version 3 with minimum reader version 3, so a build that reads version 2 but not segments refuses the file instead of missing commits; otherwise it is version 2. Releases up to v0.2.18 read none of this (they check only the magic and a checksum, and read only the first header page): the magic keeps them out. Open refuses a file that needs a newer reader or has flags it does not implement, and opens a newer format read-only",
		},
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
			name: "WAL segment table",
			value:
				"Up to 64 entries, each with a seq, start page, page count, sealed flag and byte length; plus covered, the newest segment seq the snapshot covers, and the next seq",
		},
		{
			name: "Checkpoint flag",
			value:
				"Set by an earlier version's background checkpoint between its cut and its install; this version never sets it",
		},
		{
			name: "WAL salts",
			value:
				"One per WAL region, mixed into each record's checksum and replaced whenever the WAL starts over",
		},
		{
			name: "Counters",
			value: "Max node ID, next transaction ID, last commit timestamp",
		},
		{
			name: "Checksums",
			value:
				"CRC-32 over the header fields, and one over every byte of the page but the two checksums, so a page torn between two writes, at any 512-byte sector, fails it. A header in the old magic keeps its old page checksum (over the header fields' checksum too, which leaves the fields out of it); one of those that names WAL segments, which only unreleased builds wrote, is refused with its file",
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
			text: "Build the new snapshot in memory from the current snapshot and the changes it covers",
			sub: "A blocking checkpoint uses the live delta; a background checkpoint replays the WAL segments up to its cut",
			accent: "violet",
		},
		{
			text: "Write it to pages no header names",
			sub: "The first free range that holds it, otherwise the end of the file. The current snapshot's pages are never overwritten.",
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
			text: "Write a header that points to the new snapshot, and to the log it does not cover, into the inactive header page",
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
			text: "Free the old snapshot's pages and the WAL segments the new header drops",
			sub: "Later snapshots and segments can reuse them",
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
							snapshot and the log it needs
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
					other header page. No header names the pages a crashed checkpoint
					wrote, and open frees them.
				</p>
			</div>
		</Figure>
	);
}

function WALAreaAndSegments() {
	return (
		<Figure
			title="WAL area and WAL segments"
			accent="mint"
			meta="4 MB WAL (default)"
		>
			<div class="flex overflow-hidden rounded-md border border-kite-line font-mono text-[12px]">
				<div class="min-w-0 flex-1 border-r border-kite-line bg-kite-mint/[0.07] px-3 py-2.5">
					<div class="text-slate-200">Primary</div>
					<div class="text-[11px] text-slate-500">3 MB, all writes</div>
				</div>
				<div class="w-1/4 min-w-[6.5rem] px-3 py-2.5">
					<div class="text-slate-200">Secondary</div>
					<div class="text-[11px] text-slate-500">1 MB, not written</div>
				</div>
			</div>
			<p class="mt-4 mb-2 text-[14px] text-slate-300">
				When a commit does not fit in the WAL, the WAL spills:
			</p>
			<div class="space-y-1.5">
				<FlowItem color="emerald">
					Its records are copied, unsalted, into a WAL segment: appended to the
					open extent if they fit, else into a new extent in the first free
					range that holds it, else at the end of the file
				</FlowItem>
				<FlowItem color="cyan">
					The extent is synced; then a header naming the segment and an empty
					WAL is installed durably in both header slots, and the WAL starts over
					under a fresh salt
				</FlowItem>
				<FlowItem color="violet">
					A checkpoint frees the segments it covers, but those a still-open
					write transaction has records in. Automatic checkpoints run on the
					database's checkpoint thread (or, with it or background checkpoints
					off, on the committing thread), and writers wait for one only at{" "}
					<code>walSegmentLimit</code>
				</FlowItem>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-500">
				The secondary region is only read, when opening a file from an earlier
				version, whose background checkpoints wrote there. A writable open moves
				those records back to the primary region, or into a WAL segment if they
				don't fit.
			</p>
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
		wal: "w-[5%]",
		snapshot: "w-1 bg-kite-violet/25",
		size: "~4 MB",
	},
	{
		label: "100K nodes",
		wal: "w-[5%]",
		snapshot: "w-[9%] bg-kite-violet/45",
		size: "~12 MB",
	},
	{
		label: "1M nodes",
		wal: "w-[5%]",
		snapshot: "flex-1 bg-kite-violet/45",
		size: "~90 MB",
	},
];

function FileGrowthDiagram() {
	return (
		<Figure title="File size examples" accent="cyan" meta="4 MB WAL">
			<p class="mb-4 text-[13px] text-slate-500">
				These examples assume the default 4 MB WAL and a checkpoint that covered
				the whole log, so the file holds no WAL segments, as after a clean
				close. Until a checkpoint covers them, WAL segments add to these sizes,
				up to <Code>walSegmentLimit</Code>.
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
			text: "If an earlier version left records in the secondary WAL region, move them back",
			sub: "A format version 2 background checkpoint wrote there between its cut and its install. A writable open moves those records after the primary region's, or into a WAL segment if they don't fit. Read-only opens replay both regions in place without writing.",
			accent: "amber",
		},
		{
			text: "Writable opens: free every page no header names",
			sub: "Old snapshots' pages, and extents a crash left before a header named them",
			accent: "slate",
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
			text: "Replay committed transactions from the WAL segments and the WAL, in commit order, to rebuild the delta",
			sub: "Only those whose Commit record lies after the last segment the snapshot covers",
			accent: "slate",
		},
		{ text: "Ready for queries", accent: "mint", done: true },
	];
	return (
		<Figure title="Open sequence" accent="cyan">
			<Steps steps={steps} />
			<p class="mt-5 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				<span class="text-amber-300">Incomplete transactions</span> found during
				replay are discarded, since they never committed. Recovery runs
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
				header, a write-ahead log, and a snapshot. The log is a fixed WAL area
				plus, when it fills, WAL segments elsewhere in the file. One file is
				easy to copy and deploy, and the header records where the current
				snapshot, the WAL and the WAL segments are.
			</p>

			<h2 id="file-layout">File layout</h2>

			<FileLayoutDiagram />

			<VersionNote>
				files have one header page (page 0), and the WAL starts at page 1. The
				current version converts such a file on its first writable open: it
				writes the recovered database to a temporary file and renames it over
				the original, so a crash during conversion leaves the original intact.
				Read-only opens use old files without converting them. v0.2.18 and
				earlier, which read and write only page 0, refuse a converted file: its
				headers carry a magic they do not know.
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
				The WAL area is a fixed-size, append-only log. The default size is 4 MB.
				The size is fixed when the file is created; change it with{" "}
				<code>resizeWal</code> (offline) or rebuild into a new file.
			</p>
			<p>
				A full WAL spills into a WAL segment instead of forcing a checkpoint, so
				the WAL's size no longer limits a transaction. Automatic checkpoints
				start once the log the snapshot does not cover reaches{" "}
				<code>checkpointLogRatio</code> (default 0.5) times the snapshot's size,
				at least three eighths of the WAL (where earlier releases checkpointed),
				at most <code>checkpointLogBudget</code> (default 128 MiB);{" "}
				<code>checkpointThreshold</code> is deprecated and has no effect. So the
				WAL's size still sets when a small database checkpoints, and the floors
				of the segment limit (16 WALs) and the segment extent (2 WALs); a larger
				WAL also means fewer spills. The{" "}
				<a href="/docs/internals/wal">WAL page</a> covers why spills and
				background checkpoints are crash safe.
			</p>

			<WALAreaAndSegments />

			<h2 id="snapshot-area">Snapshot area</h2>

			<p>The snapshot area holds the graph data in CSR format:</p>

			<SnapshotSections />

			<h2 id="growth">File growth</h2>

			<p>
				The header and WAL area have fixed sizes; the snapshot grows with your
				data, and WAL segments come and go with the log:
			</p>

			<FileGrowthDiagram />

			<p>
				Because a checkpoint never overwrites the current snapshot, the file can
				hold an old snapshot's pages next to the current one, with WAL segments
				between them. Snapshots and segment extents go into the first free range
				that holds them, else the end of the file, and free pages at the end of
				the file are truncated after a checkpoint. Open treats every page no
				header names as free. A clean close checkpoints the WAL segments away,
				then moves the snapshot down next to the WAL when free pages lie in
				front of it and truncates the file, as <code>vacuum</code> does. A file
				closed with a transaction open, or whose close-time checkpoint failed
				(close warns and goes on), keeps its segments where they are and is not
				compacted, as is a database dropped without closing: the next open
				replays them.
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
				never writes to the file. Replay of the WAL segments and the WAL
				rebuilds the delta in memory, and closing the database leaves the file
				untouched. In a file where an earlier version left records in the
				secondary WAL region, a read-only open replays both regions in place;
				the next writable open moves the records back.
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

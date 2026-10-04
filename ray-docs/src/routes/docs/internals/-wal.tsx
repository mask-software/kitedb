import { For, type JSX, Show } from "solid-js";
import DocPage from "~/components/doc-page";
import {
	ACCENT_DOT,
	ACCENT_TINT,
	Code,
	Figure,
	FlowItem,
	RecordTypeBadge,
	Steps,
	type Accent,
	type Step,
} from "./-components";

// ============================================================================
// SHARED DIAGRAM PIECES
// ============================================================================

/** Small tinted label, e.g. for crash outcomes and mode badges. */
function Tag(props: { accent: Accent; children: JSX.Element }) {
	return (
		<span
			class={`inline-block shrink-0 rounded-md border px-2 py-0.5 font-mono text-[11px] ${ACCENT_TINT[props.accent]}`}
		>
			{props.children}
		</span>
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
// WAL DIAGRAMS
// ============================================================================

function WALPrincipleDiagram() {
	const steps: Step[] = [
		{
			text: "Write the transaction's records and a Commit record to the WAL",
			sub: "If the WAL is full, it first spills into a WAL segment. Where an older record of this WAL cycle may lie, records go over bytes zeroed and synced first (see below)",
			accent: "slate",
		},
		{
			text: "Write the new WAL head into the inactive header page",
			accent: "slate",
		},
		{
			text: (
				<>
					<Code>fsync()</Code> the file, once for both
				</>
			),
			accent: "mint",
			note: "now durable",
		},
		{
			text: "Update the in-memory delta",
			accent: "slate",
			note: "now visible",
		},
		{ text: "Return success to the caller", accent: "slate" },
	];
	return (
		<Figure
			title="Log first, then apply"
			accent="mint"
			meta="Full sync mode (default)"
		>
			<Steps steps={steps} />
			<div class="mt-5 space-y-2 border-t border-kite-line pt-4 text-[14px]">
				<div class="flex flex-col items-start gap-1.5 sm:flex-row sm:gap-3">
					<Tag accent="mint">crash after step 3</Tag>
					<span class="text-slate-400">
						The WAL is replayed on restart and the changes are recovered.
					</span>
				</div>
				<div class="flex flex-col items-start gap-1.5 sm:flex-row sm:gap-3">
					<Tag accent="amber">crash before step 3</Tag>
					<span class="text-slate-400">
						The changes can be lost. That is allowed: the caller never received
						a success result.
					</span>
				</div>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Commits that arrive together share steps 1 to 3. A crash during the
				fsync can leave the new header on disk without some of the WAL pages it
				points at. Those pages then still hold what was there before, and KiteDB
				keeps that harmless. Records of an earlier WAL cycle fail their salt
				check. Where records of the current cycle may lie past the head (after
				reopening a file, until the WAL next starts over under a new salt at a
				spill or a checkpoint), records are only written over bytes zeroed and
				synced first, a chunk ahead of the head; in Full mode the chunk is
				topped up in the commits' own write, so it costs no extra fsync.
				Recovery reads zeros there and stops, keeping every commit acknowledged
				before, and never replays a record a torn write left behind.
			</p>
		</Figure>
	);
}

interface ByteField {
	name: string;
	size: string;
	grow: string;
	tone?: "payload" | "padding";
}

const RECORD_HEADER: ByteField[] = [
	{ name: "Length", size: "4 B", grow: "sm:grow-[4]" },
	{ name: "Type", size: "1 B", grow: "sm:grow-[2]" },
	{ name: "Flags", size: "1 B", grow: "sm:grow-[2]" },
	{ name: "Reserved", size: "2 B", grow: "sm:grow-[3]" },
	{ name: "TxID", size: "8 B", grow: "sm:grow-[6]" },
	{ name: "Payload length", size: "4 B", grow: "sm:grow-[5]" },
];

const RECORD_BODY: ByteField[] = [
	{ name: "Payload", size: "variable", grow: "sm:grow-[14]", tone: "payload" },
	{ name: "CRC-32", size: "4 B", grow: "sm:grow-[4]" },
	{ name: "Padding", size: "0–7 B", grow: "sm:grow-[4]", tone: "padding" },
];

const BYTE_TONE = {
	payload: "border-kite-mint/25 bg-kite-mint/[0.06] text-slate-100",
	padding: "border-dashed border-kite-line text-slate-400",
	default: "border-kite-line bg-white/[0.03] text-slate-200",
};

/** One row of a byte layout: a grid on mobile, proportional cells from sm up. */
function ByteRow(props: { label: string; fields: ByteField[] }) {
	return (
		<div>
			<p class="mb-1.5 font-mono text-[11px] text-slate-500">{props.label}</p>
			<div class="grid grid-cols-3 gap-1.5 font-mono text-[12px] sm:flex">
				<For each={props.fields}>
					{(field) => (
						<div
							class={`min-w-0 rounded-md border px-2.5 py-2 sm:basis-0 ${field.grow} ${BYTE_TONE[field.tone ?? "default"]}`}
						>
							<div>{field.name}</div>
							<div class="text-[11px] text-slate-500">{field.size}</div>
						</div>
					)}
				</For>
			</div>
		</div>
	);
}

function WALRecordFormat() {
	return (
		<Figure title="Record layout" accent="mint" meta="8-byte aligned">
			<div class="space-y-3">
				<ByteRow label="header, 20 bytes" fields={RECORD_HEADER} />
				<ByteRow label="body" fields={RECORD_BODY} />
			</div>
			<p class="mt-3 text-[13px] text-slate-500">
				The CRC-32 covers everything from Type through the end of the payload,
				and is XORed with the salt of the WAL region the record is in. The WAL
				gets a new salt whenever it starts over (after a spill, or a checkpoint
				that empties it), so records an earlier cycle left behind fail the
				check. WAL segments hold the same records unsalted: the header names
				each segment's byte length, so nothing past it is read. Padding brings
				each record to an 8-byte boundary.
			</p>

			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="mb-2.5 text-[13px] text-slate-500">Record types include:</p>
				<div class="flex flex-wrap gap-1.5">
					<RecordTypeBadge name="Begin" color="cyan" />
					<RecordTypeBadge name="Commit" color="emerald" />
					<RecordTypeBadge name="Rollback" color="red" />
					<For
						each={[
							"CreateNode",
							"DeleteNode",
							"AddEdge",
							"DeleteEdge",
							"SetNodeProp",
							"DelNodeProp",
						]}
					>
						{(name) => <RecordTypeBadge name={name} color="neutral" />}
					</For>
				</div>
				<p class="mt-2.5 text-[13px] text-slate-500">
					plus batch, label, schema, edge-property, and vector records.
				</p>
			</div>
		</Figure>
	);
}

function LinearBufferDiagram() {
	return (
		<Figure
			title="WAL area and WAL segments"
			accent="mint"
			meta="4 MB WAL (default)"
		>
			<div class="mb-1.5 flex justify-between gap-4 font-mono text-[11px] text-slate-500">
				<span>primary region, 75%</span>
				<span>secondary, 25%</span>
			</div>
			<div class="flex h-10 overflow-hidden rounded-md border border-kite-line font-mono text-[11px]">
				<div class="flex w-3/4 border-r border-kite-line">
					<div class="flex w-[45%] items-center border-r border-kite-mint/50 bg-kite-mint/10 px-2.5 text-kite-mint">
						records
					</div>
					<div class="flex flex-1 items-center px-2.5 text-slate-500">free</div>
				</div>
				<div class="flex w-1/4 items-center bg-white/[0.03] px-2.5 text-slate-500">
					not written
				</div>
			</div>
			<div class="relative mt-1.5 h-4 font-mono text-[11px]">
				<span class="absolute left-[33.75%] -translate-x-1/2 text-kite-mint">
					head
				</span>
			</div>

			<p class="mt-4 mb-1.5 font-mono text-[11px] text-slate-500">
				WAL segments, elsewhere in the file
			</p>
			<div class="flex gap-1.5 font-mono text-[11px]">
				<div class="min-w-0 flex-1 rounded-md border border-kite-mint/25 bg-kite-mint/[0.07] px-2.5 py-2">
					<div class="text-slate-200">seq 5</div>
					<div class="text-slate-500">sealed</div>
				</div>
				<div class="flex min-w-0 flex-1 overflow-hidden rounded-md border border-kite-line">
					<div class="w-3/5 border-r border-kite-mint/50 bg-kite-mint/[0.07] px-2.5 py-2">
						<div class="text-slate-200">seq 6</div>
						<div class="text-slate-500">open</div>
					</div>
					<div class="flex flex-1 items-center px-2.5 text-slate-500">free</div>
				</div>
			</div>

			<dl class="mt-4 space-y-1.5 text-[14px]">
				<div class="flex gap-3">
					<dt class="w-16 shrink-0 font-mono text-[13px] text-kite-mint">
						head
					</dt>
					<dd class="text-slate-400">Where the next record is written</dd>
				</div>
				<div class="flex gap-3">
					<dt class="w-16 shrink-0 font-mono text-[13px] text-kite-cyan">
						segment
					</dt>
					<dd class="text-slate-400">
						An extent of pages the header names, holding records the WAL
						spilled; read up to the byte length the header names
					</dd>
				</div>
			</dl>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Records never wrap around. The log is the WAL segments in seq order,
				then the WAL. When a commit does not fit in the WAL, the WAL spills into
				a segment and starts over, and checkpoints free the segments they cover.
				Every record is written to the primary region. The secondary region is
				only read, when opening a file from an earlier version, whose background
				checkpoints wrote there.
			</p>
		</Figure>
	);
}

function SpillDiagram() {
	const steps: Step[] = [
		{
			text: "Copy the WAL's records, unsalted, into a WAL segment",
			sub: "Appended to the open extent if they fit; otherwise into a new extent, in the first free range that holds it, else at the end of the file",
			accent: "mint",
		},
		{
			text: (
				<>
					<Code>fsync()</Code> the extent
				</>
			),
			sub: "In every sync mode, before any header names it",
			accent: "mint",
		},
		{
			text: "Install a header naming the segment and an empty WAL, durably in both header slots",
			accent: "cyan",
			note: "spilled",
		},
		{ text: "Start the WAL over under a fresh salt", accent: "slate" },
	];
	return (
		<Figure title="Spilling a full WAL" accent="mint" meta="milliseconds">
			<Steps steps={steps} />
			<div class="mt-5 space-y-2 border-t border-kite-line pt-4 text-[14px]">
				<div class="flex flex-col items-start gap-1.5 sm:flex-row sm:gap-3">
					<Tag accent="amber">crash before step 3</Tag>
					<span class="text-slate-400">
						The old header still names the records in the WAL, and no bytes of
						the segment past the length it names. No header names a new extent's
						pages, and the next open frees them.
					</span>
				</div>
				<div class="flex flex-col items-start gap-1.5 sm:flex-row sm:gap-3">
					<Tag accent="mint">crash after step 3</Tag>
					<span class="text-slate-400">
						The header names the segment, whose bytes were synced first. A
						record that fails its CRC inside a named byte range is corruption,
						not a torn tail.
					</span>
				</div>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				The writer whose commit needs the room does the spill: a copy of the WAL
				and three syncs. The WAL is not written again until both header slots
				name the new state, so whichever slot a crash leaves newest, the records
				it names are intact. A commit too large for an empty WAL, such as a
				large bulk load, goes straight to a WAL segment.
			</p>
		</Figure>
	);
}

function BackgroundCheckpointDiagram() {
	const steps: Step[] = [
		{
			text: "Cut: spill the WAL into a WAL segment and seal the newest segment",
			sub: "The new snapshot will hold every transaction committed in the segments up to this one; call its seq C",
			accent: "amber",
			note: "commit lock",
		},
		{
			text: "Replay the commits up to the cut that the installed snapshot lacks into a new delta over it",
			sub: "As recovery would, without locks. The live delta is not copied.",
			accent: "violet",
		},
		{
			text: "Build the new snapshot from the installed snapshot and that delta",
			sub: "Written to pages no header names (the first free range that holds it, else the end of the file), then synced",
			accent: "violet",
		},
		{
			text: "Replay the transactions committed after the cut over the new snapshot",
			sub: "Most without the commit lock; the last ones under it, in step 5",
			accent: "violet",
		},
		{
			text: (
				<>
					Install a header naming the new snapshot with <Code>covered = C</Code>
					, durably in both header slots
				</>
			),
			sub: "It keeps the WAL as it is, and every segment from the oldest one an open write transaction has records in. Then the dropped segments' pages and the old snapshot's pages are freed, and the new snapshot and delta are swapped in.",
			accent: "mint",
			note: "commit lock",
		},
	];
	return (
		<Figure title="Background checkpoints" accent="violet">
			<Steps steps={steps} />
			<div class="mt-5 space-y-1.5 border-t border-kite-line pt-4">
				<FlowItem color="cyan">
					Reads continue throughout, and readers keep their MVCC snapshots
					across the swap
				</FlowItem>
				<FlowItem color="emerald">
					Commits wait only while the cut and the install are written
					(milliseconds). Transactions open at the cut can commit during or
					after the checkpoint
				</FlowItem>
				<FlowItem color="emerald">
					A failure at any step loses nothing: the cut is only a spill, and
					every commit stays in the log
				</FlowItem>
				<FlowItem color="amber">
					Closing or dropping the database abandons a run still building its
					snapshot (no header names its pages, which are freed then or at the
					next open) and lets a run already installing finish
				</FlowItem>
			</div>
		</Figure>
	);
}

const SYNC_MODES: {
	name: string;
	accent: Accent;
	badge?: string;
	summary: string;
	tradeoff: string;
}[] = [
	{
		name: "Full",
		accent: "mint",
		badge: "default",
		summary:
			"One fsync per commit, or per group of commits that arrive together",
		tradeoff:
			"Safest; slowest writes. On macOS, fsync leaves writes in the drive's cache, so commits survive power loss only with fullFsync (F_FULLFSYNC, milliseconds per commit), as with SQLite",
	},
	{
		name: "Normal",
		accent: "amber",
		summary:
			"The WAL is written to the OS on every commit; fsync happens only at spills and checkpoints",
		tradeoff:
			"Much faster writes. Survives application crashes; an OS crash can lose recent commits",
	},
	{
		name: "Off",
		accent: "red",
		badge: "testing only",
		summary: "No fsync, and WAL writes are not flushed at commit",
		tradeoff: "Fastest; any crash can lose data",
	},
];

function DurabilityModes() {
	return (
		<Figure title="Sync modes" accent="mint" meta="syncMode">
			<div class="space-y-2">
				<For each={SYNC_MODES}>
					{(mode) => (
						<div class="rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
							<div class="mb-1 flex items-center gap-2.5">
								<span
									class={`h-1.5 w-1.5 rounded-full ${ACCENT_DOT[mode.accent]}`}
									aria-hidden="true"
								/>
								<span class="font-mono text-[13px] text-white">
									{mode.name}
								</span>
								<Show when={mode.badge}>
									<span class="ml-auto">
										<Tag accent={mode.accent}>{mode.badge}</Tag>
									</span>
								</Show>
							</div>
							<p class="text-[14px] text-slate-300">{mode.summary}</p>
							<p class="mt-0.5 text-[13px] text-slate-500">{mode.tradeoff}</p>
						</div>
					)}
				</For>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				For most applications, <Code>Full</Code> is the right choice. Use{" "}
				<Code>Normal</Code> when you need more write throughput and can accept
				losing recent commits on an OS crash. In either mode, commits that
				arrive together share one WAL write and one header write (and, in{" "}
				<Code>Full</Code>, one fsync).
			</p>
		</Figure>
	);
}

function RecoveryProcess() {
	const steps: Step[] = [
		{
			text: "Read both header pages and use the newest valid one to find the snapshot, the WAL and the WAL segments",
			accent: "cyan",
		},
		{
			text: "If an earlier version left records in the secondary WAL region, move them back",
			sub: "A format version 2 background checkpoint wrote there between its cut and its install. A writable open moves those records after the primary region's, or into a WAL segment if they don't fit. Read-only opens replay both regions in place without writing.",
			accent: "amber",
		},
		{
			text: "Read each WAL segment, by seq, up to the byte length the header names",
			sub: "Its bytes were synced before any header named them, so a record that fails its CRC there is corruption, and open fails, rather than a torn tail",
			accent: "cyan",
		},
		{ text: "Scan the WAL's records up to its head", accent: "cyan" },
		{
			text: "Validate each WAL record's CRC-32",
			sub: "An invalid record ends the scan: an incomplete write (a page that never landed holds zeros written before it, or an earlier cycle's bytes), or a record of an earlier WAL cycle (its salt differs)",
			accent: "violet",
		},
		{
			text: "Move the WAL head back to the last valid record",
			sub: "Writable opens save this before writing anything, so new commits never land after a torn record",
			accent: "violet",
		},
		{
			text: "Group records by transaction",
			sub: "A transaction with Begin but no Commit, or with a Rollback, is discarded",
			accent: "violet",
		},
		{
			text: (
				<>
					Replay, in commit order, the transactions whose Commit record lies
					after the last record of segment <Code>covered</Code>
				</>
			),
			sub: "Earlier commits are in the snapshot",
			accent: "mint",
		},
	];
	return (
		<Figure title="Recovery on open" accent="cyan">
			<Steps steps={steps} />
			<p class="mt-5 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Replay only rebuilds the in-memory delta, so read-only opens recover
				too, reading the segments and the WAL in place. A writable open also
				frees every page no header names, such as an extent a crash left before
				a header named it. Recovery time is{" "}
				<span class="font-mono text-slate-200">O(log size)</span>: the WAL
				segments plus the WAL. Automatic checkpoints keep the uncovered log near
				the checkpoint trigger (at most <Code>checkpointLogBudget</Code>, 128
				MiB by default) unless writers outrun them, up to{" "}
				<Code>walSegmentLimit</Code>.
			</p>
		</Figure>
	);
}

function CheckpointTriggers() {
	return (
		<Figure title="Checkpoint triggers" accent="violet">
			<p class="mb-2 text-[13px] text-slate-500">Automatic:</p>
			<ol class="space-y-2 text-[14px]">
				<li class="flex items-start gap-3">
					<span class="grid h-5 w-5 shrink-0 place-items-center rounded border border-kite-violet/30 font-mono text-[11px] text-kite-violet">
						1
					</span>
					<span class="text-slate-300">
						After a commit, when the log the snapshot does not cover (the WAL
						segments after <Code>covered</Code>, plus the WAL) reaches the
						trigger: <Code>checkpointLogRatio</Code> (default 0.5) times the
						snapshot's size, at least four WALs, at most{" "}
						<Code>checkpointLogBudget</Code> (default 128 MiB). Segments kept
						for a still-open transaction don't count
					</span>
				</li>
				<li class="flex items-start gap-3">
					<span class="grid h-5 w-5 shrink-0 place-items-center rounded border border-kite-violet/30 font-mono text-[11px] text-kite-violet">
						2
					</span>
					<span class="text-slate-300">
						When the WAL segments reach <Code>walSegmentLimit</Code> and a
						writer needs to spill
					</span>
				</li>
				<li class="flex items-start gap-3">
					<span class="grid h-5 w-5 shrink-0 place-items-center rounded border border-kite-violet/30 font-mono text-[11px] text-kite-violet">
						3
					</span>
					<span class="text-slate-300">
						On <Code>Kite</Code> close, when the uncovered log is at least{" "}
						<Code>closeCheckpointIfWalUsageAtLeast</Code> (default 0.2) of the
						trigger. This one is blocking
					</span>
				</li>
			</ol>

			<p class="mt-4 text-[14px] text-slate-300">
				<span class="text-[13px] text-slate-500">Manual:</span>{" "}
				<Code>db.checkpoint()</Code> (blocking) or{" "}
				<Code>db.backgroundCheckpoint()</Code>.{" "}
				<Code>db.shouldCheckpoint(threshold)</Code> reports whether the
				uncovered log has reached that fraction of the trigger (1.0: an
				automatic checkpoint is due).
			</p>

			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="mb-2 text-[13px] text-slate-500">
					Automatic checkpoints (background, the default):
				</p>
				<div class="space-y-1.5">
					<FlowItem color="violet">
						Run on the database's checkpoint thread,{" "}
						<Code>kitedb-checkpoint</Code>, started at the first automatic
						checkpoint, so the commit that crosses the trigger returns at once.
						Read-only opens have no such thread; with{" "}
						<Code>checkpointThread: false</Code>, without background
						checkpoints, or on wasm32, checkpoints run on the committing thread
					</FlowItem>
					<FlowItem color="cyan">
						Reads and writes continue while one runs (see Background checkpoints
						above)
					</FlowItem>
					<FlowItem color="red">
						A failure is logged, recorded and returned by{" "}
						<Code>checkpointError()</Code> until a checkpoint installs. The
						thread retries at the next trigger, after a wait that starts at 1 s
						and doubles up to 60 s
					</FlowItem>
				</div>
				<p class="mt-3 text-[13px] text-slate-500">
					A blocking checkpoint, such as <Code>db.checkpoint()</Code>, builds
					from the live delta, makes new transactions wait for its whole run and
					lets open ones finish first. It installs a header with an empty WAL
					and no WAL segments (format version 2 again); <Code>optimize</Code>,{" "}
					<Code>vacuum</Code> and <Code>resizeWal</Code> also leave no segments.
					If installing its header fails, it returns an error and the database
					keeps the previous snapshot and log, so later commits append after the
					existing records. Closing keeps live segments, and the next open
					replays them.
				</p>
			</div>
		</Figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function WALPage() {
	return (
		<DocPage slug="internals/wal">
			<p>
				The write-ahead log (WAL) makes committed transactions survive crashes.
				Before a transaction counts as committed, its changes must be written to
				the WAL and flushed to disk.
			</p>

			<h2 id="principle">The WAL principle</h2>

			<WALPrincipleDiagram />

			<h2 id="record-format">WAL record format</h2>

			<p>Each operation is stored as a framed record:</p>

			<WALRecordFormat />

			<h2 id="circular-buffer">Linear buffer</h2>

			<p>
				The WAL area has a fixed size. Records are appended to it until it is
				full; then they move into a WAL segment, an extent of pages elsewhere in
				the file, and the WAL starts over:
			</p>

			<LinearBufferDiagram />

			<h2 id="segments">Spilling into WAL segments</h2>

			<p>
				When the WAL fills, the database does not force a checkpoint. It spills
				the WAL into a WAL segment:
			</p>

			<SpillDiagram />

			<p>
				The header names up to 64 segments. Each entry holds the segment's seq,
				start page, page count, byte length, and whether it is sealed (no spill
				appends to it any more). The table also holds <code>covered</code>, the
				newest segment seq the snapshot covers, and the seq the next segment
				gets. A header that names WAL segments is written as format version 3,
				with minimum reader version 3, so older versions refuse the file with a
				version mismatch instead of missing the commits in its segments. Once a
				checkpoint covers every segment, the header is written as version 2
				again.
			</p>

			<VersionNote>
				the WAL never spilled. Automatic checkpoints ran on the committing
				thread whenever the active WAL region reached{" "}
				<code>checkpointThreshold</code>, and a commit too large for the WAL
				failed with <code>WAL buffer full</code>.
			</VersionNote>

			<h2 id="background-checkpoints">Background checkpoints</h2>

			<p>
				A background checkpoint folds the log into a new snapshot while reads
				and writes continue. Automatic checkpoints run this way on the
				database's checkpoint thread. <code>backgroundCheckpoint()</code> runs
				one on the calling thread, after any running one ends, and returns after
				its install.
			</p>

			<BackgroundCheckpointDiagram />

			<VersionNote>
				transactions that committed while a background checkpoint was running
				could stay invisible to reads until the database was reopened.
			</VersionNote>

			<h2 id="fsync">Durability guarantees</h2>

			<p>
				Durability is configurable with the <code>syncMode</code> option:
			</p>

			<DurabilityModes />

			<h2 id="fast-writes">Fast writes (single-file)</h2>

			<p>Recommended profile for high write throughput:</p>

			<ul>
				<li>
					<code>syncMode = Normal</code>
				</li>
				<li>
					Commit from several threads: commits that arrive while others are
					written are written together, with one WAL write, one header write and
					(in <code>Full</code> mode) one fsync. This is always on;{" "}
					<code>groupCommitEnabled</code> has no effect, and no commit waits for
					others to join
				</li>
				<li>
					<code>beginBulk()</code> + batch APIs for ingest (with or without
					MVCC)
				</li>
				<li>
					The default 4 MB WAL is enough for heavy ingest: a full WAL spills
					into a WAL segment, and checkpoints run on the checkpoint thread
					without holding up commits. A larger <code>walSize</code> only means
					fewer spills (each is a copy of the WAL and three syncs)
				</li>
			</ul>

			<div class="my-6 rounded-lg border border-amber-400/20 bg-amber-400/[0.05] px-4 py-3 text-[14px] text-slate-300">
				<strong>Durability note:</strong> <code>Normal</code> mode does not{" "}
				<code>fsync</code> on every commit. An OS crash can lose recent commits,
				but application crashes are recovered via WAL replay.
			</div>

			<h2 id="recovery">Crash recovery</h2>

			<p>
				On database open, the log (the WAL segments, then the WAL) is replayed
				to rebuild the delta:
			</p>

			<RecoveryProcess />

			<VersionNote>
				committed transactions were replayed in arbitrary order instead of
				commit order, so the state after a crash could differ from the state
				before it. Read-only opens wrote to the file when they found an
				interrupted background checkpoint.
			</VersionNote>

			<h2 id="checkpoint-trigger">When checkpoints happen</h2>

			<CheckpointTriggers />

			<p>
				The in-memory delta takes about ten times the log's bytes, so the log
				budget bounds memory: the default 128 MiB of log is about 1.3 GB of
				delta at most at the trigger. A checkpoint briefly needs about twice
				that, for the delta it replays from the cut plus the live delta. To
				bound reopen replay time or memory, lower{" "}
				<code>checkpointLogBudget</code>. To checkpoint less often on a large
				database, raise <code>checkpointLogRatio</code>, at the cost of more
				memory; the budget caps the trigger either way.{" "}
				<code>checkpointThreshold</code> is deprecated and has no effect.
			</p>

			<p>
				<code>walSegmentLimit</code> (default: twice the trigger, at least 16
				WALs, at most four times <code>checkpointLogBudget</code>) bounds the
				bytes of WAL segments, and with them disk use and the delta's memory.
				Writers wait only when the segments reach it: a writer that needs to
				spill then asks for a checkpoint and waits for its install to free
				segments.
			</p>

			<h2 id="overflow">Avoiding WAL overflow</h2>

			<p>
				A full WAL does not fail a write: it spills into a WAL segment, and a
				commit too large for an empty WAL goes straight to one. A write fails
				with <code>WAL buffer full</code> (<code>WalBufferFull</code>) only when
				the WAL segments reach <code>walSegmentLimit</code> and waiting for a
				checkpoint cannot help:
			</p>
			<ul>
				<li>
					Automatic checkpoints are off: spills continue up to the limit, then
					writes fail. Run <code>checkpoint()</code> before the limit, or raise{" "}
					<code>walSegmentLimit</code>.
				</li>
				<li>
					Open write transactions hold records in enough segments to fill the
					limit. No checkpoint can free those until the transactions finish:
					keep write transactions shorter, or raise <code>walSegmentLimit</code>
					.
				</li>
				<li>
					A blocking checkpoint, <code>optimize</code>, <code>vacuum</code> or{" "}
					<code>resizeWal</code> is waiting for the writer's own transaction to
					finish.
				</li>
				<li>The checkpoint the writer waited for freed no segment space.</li>
			</ul>
			<p>
				While the last automatic checkpoint failed, a writer at the limit fails
				instead of waiting, with{" "}
				<code>Checkpoint failed, and the WAL segments are full: ...</code> (
				<code>CheckpointFailed</code>; Python raises{" "}
				<code>CheckpointError</code>, a <code>KiteError</code> subclass), and
				asks for another checkpoint, which the thread runs after its back-off.{" "}
				<code>checkpointError()</code> (<code>checkpoint_error()</code> in
				Python) returns the checkpoint's error. A panic in a checkpoint run is
				reported the same way; since it may have struck between writes that keep
				memory and disk in step, the handle then refuses writes (
				<code>The database refuses writes until it is reopened: ...</code>,{" "}
				<code>WritesRefused</code>) and the thread ends. Reads go on, and
				reopening the database recovers every acknowledged commit from disk. A
				rollback never waits or
				fails for log space: its record is not needed, since recovery drops a
				transaction without a commit record.
			</p>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/single-file">Single-file format</a>: how the
					WAL and its segments fit in the file layout
				</li>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot and delta</a>: what
					a checkpoint produces
				</li>
				<li>
					<a href="/docs/internals/mvcc">MVCC and transactions</a>: how
					transactions work
				</li>
			</ul>
		</DocPage>
	);
}

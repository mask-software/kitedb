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
			sub: "Over bytes already zeroed and synced (see below)",
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
				keeps that harmless: it only writes records over bytes it zeroed and
				synced first, a chunk ahead of the head (topped up in the same write, so
				it costs no extra fsync in steady state). Recovery reads zeros there and
				stops, keeping every commit acknowledged before, and never replays a
				record a torn write or an earlier WAL cycle left behind.
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
				and is XORed with the salt of the WAL region the record is in. A region
				gets a new salt whenever a checkpoint empties it for reuse, so records
				an earlier cycle left behind fail the check. Padding brings each record
				to an 8-byte boundary.
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
		<Figure title="WAL area" accent="mint" meta="64 MB example">
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
					idle
				</div>
			</div>
			<div class="relative mt-1.5 h-4 font-mono text-[11px]">
				<span class="absolute left-0 text-kite-cyan">tail</span>
				<span class="absolute left-[33.75%] -translate-x-1/2 text-kite-mint">
					head
				</span>
			</div>

			<dl class="mt-4 space-y-1.5 text-[14px]">
				<div class="flex gap-3">
					<dt class="w-10 shrink-0 font-mono text-[13px] text-kite-mint">
						head
					</dt>
					<dd class="text-slate-400">Where the next record is written</dd>
				</div>
				<div class="flex gap-3">
					<dt class="w-10 shrink-0 font-mono text-[13px] text-kite-cyan">
						tail
					</dt>
					<dd class="text-slate-400">
						First record not yet checkpointed; replay starts here
					</dd>
				</div>
			</dl>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Records never wrap around. When the active region reaches the checkpoint
				threshold (50% by default), KiteDB runs a checkpoint, and the tail moves
				past the records the new snapshot covers. A blocking checkpoint resets
				the WAL to empty. A write that does not fit fails with a{" "}
				<span class="text-amber-300">WAL buffer full</span> error.
			</p>
		</Figure>
	);
}

function WALDualRegionDetailed() {
	return (
		<Figure title="Background checkpoints" accent="violet">
			<p class="mb-2 text-[14px] text-slate-300">
				When a background checkpoint starts:
			</p>
			<ol class="mb-4 space-y-1 text-[14px] text-slate-400">
				<For
					each={[
						"Pending WAL writes are flushed and fsynced.",
						"New writes switch to the secondary region, and the header's checkpoint flag is set and fsynced.",
						"The new snapshot is built from the current snapshot and the delta, which already hold every change in the primary region.",
					]}
				>
					{(item, i) => (
						<li class="flex gap-2.5">
							<span class="pt-px font-mono text-[12px] text-slate-500">
								{i() + 1}
							</span>
							<span>{item}</span>
						</li>
					)}
				</For>
			</ol>

			<div class="flex overflow-hidden rounded-md border border-kite-line">
				<div class="min-w-0 flex-1 border-r border-kite-line bg-kite-violet/[0.07] px-3 py-2.5">
					<div class="font-mono text-[12px] text-slate-200">Primary, 75%</div>
					<div class="mt-0.5 text-[12px] text-slate-500">
						Covered by the new snapshot
					</div>
				</div>
				<div class="w-1/4 min-w-[6.5rem] bg-kite-mint/[0.07] px-3 py-2.5">
					<div class="font-mono text-[12px] text-slate-200">Secondary, 25%</div>
					<div class="mt-0.5 text-[12px] text-slate-500">
						New writes go here
					</div>
				</div>
			</div>

			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="mb-2 text-[13px] text-slate-500">
					After the checkpoint completes:
				</p>
				<div class="space-y-1.5">
					<FlowItem color="emerald">
						The primary region's records are covered by the new snapshot and are
						no longer needed for replay
					</FlowItem>
					<FlowItem color="emerald">
						The header that installs the new snapshot no longer points at any
						record written before the checkpoint started, so that space is free
						again. If nothing committed during the checkpoint, the WAL is empty.
						Otherwise those commits stay in the secondary region until the new
						header is durable, then are rewritten at the start of the primary
						region, and new writes continue after them.
					</FlowItem>
					<FlowItem color="emerald">
						Transactions that committed during the checkpoint stay visible
					</FlowItem>
				</div>
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
			"The WAL is written to the OS on every commit; fsync happens only at checkpoint",
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
			text: "Read both header pages and use the newest valid one to find the WAL boundaries",
			accent: "cyan",
		},
		{
			text: "If a background checkpoint was interrupted, finish or undo its cut",
			sub: "If the secondary region's records fit after the primary's, a writable open appends them; otherwise both regions are replayed in place and the next background checkpoint resumes the cut. Read-only opens replay in place without writing.",
			accent: "amber",
		},
		{ text: "Scan records from tail to head", accent: "cyan" },
		{
			text: "Validate each record's CRC-32",
			sub: "An invalid record ends the scan: an incomplete write (pages that never landed read as the zeros written before them), or a record of an earlier WAL cycle (its salt differs)",
			accent: "violet",
		},
		{
			text: "Move each WAL head back to the last valid record",
			sub: "Writable opens save this before writing anything, so new commits never land after a torn record",
			accent: "violet",
		},
		{
			text: "Group records by transaction",
			sub: "A transaction with Begin but no Commit, or with a Rollback, is discarded",
			accent: "violet",
		},
		{
			text: "Replay committed transactions into the delta, in the order of their Commit records",
			accent: "mint",
		},
	];
	return (
		<Figure title="Recovery on open" accent="cyan">
			<Steps steps={steps} />
			<p class="mt-5 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Replay only rebuilds the in-memory delta, so read-only opens recover
				too. Recovery time is{" "}
				<span class="font-mono text-slate-200">O(WAL size)</span>, typically
				under one second.
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
						After a commit, when the active WAL region reaches{" "}
						<Code>checkpointThreshold</Code> (default 0.5)
					</span>
				</li>
				<li class="flex items-start gap-3">
					<span class="grid h-5 w-5 shrink-0 place-items-center rounded border border-kite-violet/30 font-mono text-[11px] text-kite-violet">
						2
					</span>
					<span class="text-slate-300">
						On close, when WAL usage is at least{" "}
						<Code>closeCheckpointIfWalUsageAtLeast</Code> (default 0.2)
					</span>
				</li>
			</ol>

			<p class="mt-4 text-[14px] text-slate-300">
				<span class="text-[13px] text-slate-500">Manual:</span>{" "}
				<Code>db.checkpoint()</Code>
			</p>

			<div class="mt-5 border-t border-kite-line pt-4">
				<p class="mb-2 text-[13px] text-slate-500">
					During a background checkpoint (the default):
				</p>
				<div class="space-y-1.5">
					<FlowItem color="cyan">
						Reads continue, from the old snapshot plus the delta
					</FlowItem>
					<FlowItem color="emerald">
						Writes continue, into the secondary WAL region
					</FlowItem>
					<FlowItem color="amber">
						It starts even while other threads have write transactions open:
						their WAL records so far are copied into the secondary region, and
						they can commit during or after the checkpoint. New transactions
						pause only for the brief start and the header install. If the open
						transactions' records don't fit in the secondary region, the
						checkpoint is skipped until one of them finishes. If the secondary
						region fills before the checkpoint installs, writers wait for the
						install instead of failing.
					</FlowItem>
				</div>
				<p class="mt-3 text-[13px] text-slate-500">
					A blocking checkpoint, such as <Code>db.checkpoint()</Code>, makes new
					transactions wait for its whole run and lets open ones finish first.
					If installing its header fails, it returns an error and the database
					keeps the previous snapshot and WAL, so later commits append after the
					existing records.
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
				The WAL area has a fixed size. Records are appended to the active region
				until a checkpoint folds them into the snapshot and frees the space:
			</p>

			<LinearBufferDiagram />

			<h2 id="dual-region">Dual-region design</h2>

			<p>The WAL is split into primary (75%) and secondary (25%) regions:</p>

			<WALDualRegionDetailed />

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
					Optional: increase <code>walSizeMb</code> (e.g., 64 MB) for heavy
					ingest to reduce checkpoints
				</li>
			</ul>

			<div class="my-6 rounded-lg border border-amber-400/20 bg-amber-400/[0.05] px-4 py-3 text-[14px] text-slate-300">
				<strong>Durability note:</strong> <code>Normal</code> mode does not{" "}
				<code>fsync</code> on every commit. An OS crash can lose recent commits,
				but application crashes are recovered via WAL replay.
			</div>

			<h2 id="recovery">Crash recovery</h2>

			<p>On database open, the WAL is replayed to rebuild the delta:</p>

			<RecoveryProcess />

			<VersionNote>
				committed transactions were replayed in arbitrary order instead of
				commit order, so the state after a crash could differ from the state
				before it. Read-only opens wrote to the file when they found an
				interrupted background checkpoint.
			</VersionNote>

			<h2 id="checkpoint-trigger">When checkpoints happen</h2>

			<CheckpointTriggers />

			<h2 id="overflow">Avoiding WAL overflow</h2>

			<p>
				The WAL has a fixed size once the file is created. For large ingests,
				use <code>resizeWal</code> (offline) to grow it, or rebuild into a new
				file. To prevent single transactions from overfilling the active WAL
				region, split work into smaller commits (see <code>bulkWrite</code> or
				chunked <code>beginBulk()</code> sessions) and consider disabling
				background checkpoints during ingest.
			</p>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/single-file">Single-file format</a>: how the
					WAL fits in the file layout
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

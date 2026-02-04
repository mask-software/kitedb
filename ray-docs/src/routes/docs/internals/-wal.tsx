import DocPage from "~/components/doc-page";
import { RecordTypeBadge } from "./-components";

// ============================================================================
// WAL-SPECIFIC COMPONENTS
// ============================================================================

// WAL Principle diagram
function WALPrincipleDiagram() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z"
					/>
				</svg>
				<h4 class="font-semibold text-emerald-400">Rule: Log before you do</h4>
			</div>

			<div class="space-y-2 mb-5">
				<WALStep num={1} text="Write all changes to WAL" />
				<WALStep
					num={2}
					text="fsync() WAL to disk"
					highlight
					note="Data is now durable"
				/>
				<WALStep
					num={3}
					text="Update in-memory delta"
					note="Data is now visible"
				/>
				<WALStep num={4} text="Return success to caller" />
			</div>

			{/* Crash scenarios */}
			<div class="pt-4 border-t border-slate-700/50 space-y-2">
				<div class="flex items-start gap-3 text-sm">
					<span class="px-2 py-0.5 rounded bg-emerald-500/20 text-emerald-400 text-xs font-medium shrink-0">
						After step 2
					</span>
					<span class="text-slate-400">
						Replay WAL on restart → changes recovered
					</span>
				</div>
				<div class="flex items-start gap-3 text-sm">
					<span class="px-2 py-0.5 rounded bg-amber-500/20 text-amber-400 text-xs font-medium shrink-0">
						Before step 2
					</span>
					<span class="text-slate-400">
						Changes lost, but OK (transaction didn't commit)
					</span>
				</div>
			</div>
		</div>
	);
}

function WALStep(props: {
	num: number;
	text: string;
	highlight?: boolean;
	note?: string;
}) {
	return (
		<div class="flex items-center gap-3">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 ${props.highlight ? "bg-emerald-500/30 text-emerald-400" : "bg-slate-700 text-slate-400"}`}
			>
				{props.num}
			</span>
			<span
				class={`text-sm ${props.highlight ? "text-emerald-300 font-medium" : "text-slate-300"}`}
			>
				{props.text}
			</span>
			{props.note && (
				<span class="text-xs text-slate-500 ml-auto">← {props.note}</span>
			)}
		</div>
	);
}

// WAL Record Format visualization
function WALRecordFormat() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center gap-2 mb-4">
				<svg
					class="w-5 h-5 text-violet-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
					/>
				</svg>
				<h4 class="font-semibold text-violet-400">WAL Record Format</h4>
			</div>

			{/* Record structure */}
			<div class="space-y-0 mb-5">
				<div class="rounded-t-lg border-2 border-cyan-500/30 bg-cyan-500/10 px-4 py-2 flex justify-between items-center">
					<span class="text-sm text-cyan-400 font-medium">Length</span>
					<span class="text-xs text-slate-500 font-mono">4 bytes</span>
				</div>
				<div class="border-2 border-t-0 border-violet-500/30 bg-violet-500/5 px-4 py-2">
					<div class="grid grid-cols-2 gap-2 text-sm">
						<div class="flex justify-between">
							<span class="text-violet-300">Type</span>
							<span class="text-xs text-slate-500 font-mono">1 byte</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">Flags</span>
							<span class="text-xs text-slate-500 font-mono">1 byte</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">Reserved</span>
							<span class="text-xs text-slate-500 font-mono">2 bytes</span>
						</div>
						<div class="flex justify-between">
							<span class="text-violet-300">TxID</span>
							<span class="text-xs text-slate-500 font-mono">8 bytes</span>
						</div>
					</div>
				</div>
				<div class="border-2 border-t-0 border-slate-600/50 bg-slate-800/50 px-4 py-3 flex justify-between items-center">
					<span class="text-sm text-slate-300">Payload</span>
					<span class="text-xs text-slate-500 font-mono">variable</span>
				</div>
				<div class="rounded-b-lg border-2 border-t-0 border-emerald-500/30 bg-emerald-500/10 px-4 py-2 flex justify-between items-center">
					<span class="text-sm text-emerald-400 font-medium">
						CRC32C + Padding
					</span>
					<span class="text-xs text-slate-500 font-mono">align to 8</span>
				</div>
			</div>

			{/* Record types */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">Record Types:</p>
				<div class="flex flex-wrap gap-2">
					<RecordTypeBadge name="BEGIN" color="cyan" />
					<RecordTypeBadge name="COMMIT" color="emerald" />
					<RecordTypeBadge name="ROLLBACK" color="red" />
					<RecordTypeBadge name="CREATE_NODE" color="violet" />
					<RecordTypeBadge name="DELETE_NODE" color="violet" />
					<RecordTypeBadge name="ADD_EDGE" color="violet" />
					<RecordTypeBadge name="DELETE_EDGE" color="violet" />
					<RecordTypeBadge name="SET_NODE_PROP" color="violet" />
					<RecordTypeBadge name="DEL_NODE_PROP" color="violet" />
				</div>
			</div>
		</div>
	);
}

// Circular buffer visualization
function CircularBufferDiagram() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<div class="flex items-center justify-between mb-4">
				<h4 class="font-semibold text-cyan-400">Circular Buffer</h4>
				<span class="text-xs text-slate-500 font-mono">64 MB</span>
			</div>

			{/* Visual buffer */}
			<div class="mb-4">
				<div class="h-8 rounded-lg overflow-hidden flex border border-slate-600">
					<div class="w-1/4 bg-slate-700/50 flex items-center justify-center">
						<span class="text-xs text-slate-500">reclaimed</span>
					</div>
					<div class="w-1/6 bg-cyan-500/30 flex items-center justify-center border-l-2 border-cyan-400">
						<span class="text-xs text-cyan-400 font-medium">TAIL</span>
					</div>
					<div class="flex-1 bg-slate-800 flex items-center justify-center">
						<span class="text-xs text-slate-500">free space</span>
					</div>
					<div class="w-1/4 bg-violet-500/30 flex items-center justify-center border-l-2 border-violet-400">
						<span class="text-xs text-violet-400 font-medium">HEAD</span>
					</div>
				</div>
			</div>

			{/* Legend */}
			<div class="space-y-1.5 text-sm">
				<div class="flex items-center gap-3">
					<span class="text-violet-400 font-medium w-12">HEAD</span>
					<span class="text-slate-400">Where new records are written</span>
				</div>
				<div class="flex items-center gap-3">
					<span class="text-cyan-400 font-medium w-12">TAIL</span>
					<span class="text-slate-400">
						Start of unprocessed records (for replay)
					</span>
				</div>
			</div>

			{/* Note */}
			<div class="mt-4 pt-3 border-t border-slate-700/50">
				<p class="text-xs text-amber-400">
					When HEAD catches up to TAIL → Trigger checkpoint to free space
				</p>
			</div>
		</div>
	);
}

// Dual region explanation
function WALDualRegionDetailed() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">Why Two Regions?</h4>

			{/* Problem */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-sm text-slate-400">
					<span class="text-amber-400 font-medium">During checkpoint:</span>
				</p>
				<ul class="mt-2 space-y-1 text-sm text-slate-400 ml-4">
					<li>
						1. Primary region is being <span class="text-cyan-400">READ</span>{" "}
						to build new snapshot
					</li>
					<li>
						2. New transactions need somewhere to{" "}
						<span class="text-violet-400">WRITE</span>
					</li>
				</ul>
			</div>

			{/* Solution visual */}
			<div class="mb-4">
				<div class="flex rounded-lg overflow-hidden border border-violet-500/30">
					<div class="w-3/4 bg-violet-500/20 p-3 border-r border-violet-500/30">
						<div class="text-sm font-medium text-violet-300">Primary (75%)</div>
						<div class="text-xs text-slate-400 mt-1">
							Being read for checkpoint
						</div>
					</div>
					<div class="w-1/4 bg-emerald-500/20 p-3">
						<div class="text-sm font-medium text-emerald-300">
							Secondary (25%)
						</div>
						<div class="text-xs text-slate-400 mt-1">New writes go here</div>
					</div>
				</div>
			</div>

			{/* After checkpoint */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">After checkpoint completes:</p>
				<ul class="space-y-1 text-sm text-slate-400">
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Primary is cleared (data is in new snapshot)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">•</span>
						Secondary becomes the new primary
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">→</span>
						<span class="text-emerald-400">
							Writes continue without interruption
						</span>
					</li>
				</ul>
			</div>
		</div>
	);
}

// Durability modes comparison
function DurabilityModes() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Sync Modes</h4>

			<div class="space-y-3">
				{/* Full */}
				<div class="rounded-lg border border-emerald-500/30 bg-emerald-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-emerald-400">full</span>
						<span class="text-xs text-emerald-400/70 px-2 py-0.5 rounded bg-emerald-500/20">
							default
						</span>
					</div>
					<p class="text-sm text-slate-400">fsync every commit</p>
					<p class="text-xs text-slate-500 mt-1">Safest, slower writes</p>
				</div>

				{/* Batch */}
				<div class="rounded-lg border border-amber-500/30 bg-amber-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-amber-400">batch</span>
					</div>
					<p class="text-sm text-slate-400">fsync every N commits or T ms</p>
					<p class="text-xs text-slate-500 mt-1">
						Better throughput, small loss window
					</p>
				</div>

				{/* Off */}
				<div class="rounded-lg border border-red-500/30 bg-red-500/5 p-3">
					<div class="flex items-center justify-between mb-1">
						<span class="font-medium text-red-400">off</span>
						<span class="text-xs text-red-400/70 px-2 py-0.5 rounded bg-red-500/20">
							danger
						</span>
					</div>
					<p class="text-sm text-slate-400">No fsync (OS decides)</p>
					<p class="text-xs text-slate-500 mt-1">Fastest, data loss on crash</p>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				For most applications, <span class="text-emerald-400">full</span> is the
				right choice. Use <span class="text-amber-400">batch</span> for high
				write throughput with acceptable risk.
			</p>
		</div>
	);
}

// Recovery process visualization
function RecoveryProcess() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Recovery Process</h4>

			<div class="relative">
				{/* Vertical line */}
				<div class="absolute left-3 top-3 bottom-12 w-px bg-gradient-to-b from-cyan-500/50 via-violet-500/50 to-emerald-500/50" />

				<div class="space-y-2.5">
					<RecoveryStep num={1} text="Read header to find WAL boundaries" />
					<RecoveryStep num={2} text="Scan from TAIL to HEAD" />
					<RecoveryStep num={3} text="For each record: validate CRC32C" />
					<RecoveryStep
						num={4}
						text="If valid → apply to delta"
						sub="If invalid → stop (incomplete write)"
					/>
					<RecoveryStep
						num={5}
						text="Handle incomplete transactions"
						sub="BEGIN without COMMIT → discard"
					/>
				</div>
			</div>

			{/* Performance note */}
			<div class="mt-4 pt-3 border-t border-slate-700/50 flex items-center gap-2">
				<svg
					class="w-4 h-4 text-emerald-400"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M13 10V3L4 14h7v7l9-11h-7z"
					/>
				</svg>
				<p class="text-xs text-slate-400">
					Recovery time:{" "}
					<span class="text-emerald-400 font-mono">O(WAL size)</span>, typically
					&lt; 1 second
				</p>
			</div>
		</div>
	);
}

function RecoveryStep(props: { num: number; text: string; sub?: string }) {
	const bgColor = () => {
		if (props.num <= 2) return "bg-cyan-500/20 text-cyan-400";
		if (props.num <= 4) return "bg-violet-500/20 text-violet-400";
		return "bg-emerald-500/20 text-emerald-400";
	};
	return (
		<div class="flex items-start gap-3 relative">
			<span
				class={`flex items-center justify-center w-6 h-6 rounded-full text-xs font-bold shrink-0 z-10 ${bgColor()}`}
			>
				{props.num}
			</span>
			<div>
				<span class="text-sm text-slate-300">{props.text}</span>
				{props.sub && <p class="text-xs text-slate-500 mt-0.5">{props.sub}</p>}
			</div>
		</div>
	);
}

// Checkpoint triggers
function CheckpointTriggers() {
	return (
		<div class="my-6 rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-emerald-400 mb-4">
				When Checkpoint Happens
			</h4>

			{/* Automatic triggers */}
			<div class="mb-4">
				<p class="text-xs text-slate-500 mb-2">Automatic triggers:</p>
				<div class="space-y-2">
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							1
						</span>
						<span class="text-slate-300">WAL reaches 75% capacity</span>
					</div>
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							2
						</span>
						<span class="text-slate-300">
							Configured time interval (e.g., every 5 minutes)
						</span>
					</div>
					<div class="flex items-center gap-3 text-sm">
						<span class="w-5 h-5 rounded bg-violet-500/20 text-violet-400 flex items-center justify-center text-xs font-bold">
							3
						</span>
						<span class="text-slate-300">On graceful shutdown</span>
					</div>
				</div>
			</div>

			{/* Manual */}
			<div class="mb-4 p-3 rounded-lg bg-slate-800/50 border border-slate-700">
				<p class="text-xs text-slate-500 mb-1">Manual checkpoint:</p>
				<code class="text-sm text-cyan-400 font-mono">
					await db.optimize();
				</code>
			</div>

			{/* During checkpoint */}
			<div class="pt-4 border-t border-slate-700/50">
				<p class="text-xs text-slate-500 mb-2">During checkpoint:</p>
				<ul class="space-y-1 text-sm text-slate-400">
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						Reads continue (from old snapshot + delta)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						Writes continue (to secondary WAL region)
					</li>
					<li class="flex items-center gap-2">
						<span class="text-emerald-400">✓</span>
						<span class="text-emerald-400 font-medium">No downtime</span>
					</li>
				</ul>
			</div>
		</div>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function WALPage() {
	return (
		<DocPage slug="internals/wal">
			<p>
				The Write-Ahead Log (WAL) ensures that committed transactions survive
				crashes. Before any data is considered committed, it must be written to
				the WAL and flushed to disk.
			</p>

			<h2 id="principle">The WAL Principle</h2>

			<WALPrincipleDiagram />

			<h2 id="record-format">WAL Record Format</h2>

			<p>Each operation is stored as a framed record:</p>

			<WALRecordFormat />

			<h2 id="circular-buffer">Circular Buffer</h2>

			<p>
				The WAL is a fixed-size circular buffer. When it fills up, old (already
				checkpointed) data is overwritten:
			</p>

			<CircularBufferDiagram />

			<h2 id="dual-region">Dual-Region Design</h2>

			<p>The WAL is split into primary (75%) and secondary (25%) regions:</p>

			<WALDualRegionDetailed />

			<h2 id="fsync">Durability Guarantees</h2>

			<p>KiteDB provides configurable durability:</p>

			<DurabilityModes />

			<h2 id="fast-writes">Fast Writes (Single-File)</h2>

			<p>Recommended profile for high write throughput:</p>

			<ul>
				<li>
					<code>sync_mode = Normal</code>
				</li>
				<li>
					<code>group_commit_enabled = true</code>
				</li>
				<li>
					<code>group_commit_window_ms = 2</code>
				</li>
				<li>
					<code>begin_bulk()</code> + batch APIs for ingest (MVCC disabled)
				</li>
				<li>
					Optional: increase <code>wal_size</code> (e.g., 64MB) for heavy ingest to
					reduce checkpoints
				</li>
			</ul>

			<p class="text-sm text-slate-400">
				Durability note: <code>Normal</code> mode does not <code>fsync</code> on every
				commit. An OS crash can lose recent commits, but application crashes are
				recovered via WAL replay.
			</p>

			<h2 id="recovery">Crash Recovery</h2>

			<p>On database open, the WAL is replayed to rebuild the delta:</p>

			<RecoveryProcess />

			<h2 id="checkpoint-trigger">When Checkpoint Happens</h2>

			<CheckpointTriggers />

			<h2 id="overflow">Avoiding WAL Overflow</h2>

			<p>
				The WAL has a fixed size once the file is created. For large ingests,
				use <code>resizeWal</code> (offline) to grow it, or rebuild into a new
				file. To prevent single transactions from overfilling the active WAL
				region, split work into smaller commits (see <code>bulkWrite</code> or
				chunked <code>begin_bulk()</code> sessions) and consider disabling
				background checkpoints during ingest.
			</p>

			<h2 id="next">Next Steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/single-file">Single-File Format</a> – How WAL
					fits in the file layout
				</li>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – What
					checkpoint produces
				</li>
				<li>
					<a href="/docs/internals/mvcc">MVCC & Transactions</a> – How
					transactions work
				</li>
			</ul>
		</DocPage>
	);
}

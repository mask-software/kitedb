import { ArrowLeft } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";
import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";
import {
	ACCENT_DOT,
	Code,
	Figure,
	FlowItem,
	StepNumber,
	type Accent,
} from "./-components";

// ============================================================================
// SHARED DIAGRAM PIECES
// ============================================================================

/** Short note describing how v0.2.18 and earlier behaved. */
function VersionNote(props: { children: JSX.Element }) {
	return (
		<div class="my-6 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3 text-[14px] text-slate-400">
			<span class="text-slate-200">v0.2.18 and earlier:</span> {props.children}
		</div>
	);
}

// ============================================================================
// MVCC DIAGRAMS
// ============================================================================

const TIMELINE_EVENTS: {
	label: string;
	detail: string;
	accent: Accent;
}[] = [
	{ label: "T1 starts", detail: "sees v1", accent: "cyan" },
	{ label: "T2 starts", detail: "sees v1", accent: "cyan" },
	{ label: "T1 commits", detail: "writes v2", accent: "mint" },
];

function SnapshotIsolationTimeline() {
	return (
		<Figure title="Two overlapping transactions" accent="cyan">
			<div class="relative mb-5">
				<span
					class="absolute top-[5px] right-[16.67%] left-[16.67%] h-px bg-kite-line"
					aria-hidden="true"
				/>
				<div class="relative grid grid-cols-3 text-center">
					<For each={TIMELINE_EVENTS}>
						{(event) => (
							<div class="flex flex-col items-center">
								<span
									class={`mb-2.5 h-[11px] w-[11px] rounded-full border-2 border-kite-bg ${ACCENT_DOT[event.accent]}`}
								/>
								<span class="text-[13px] font-medium text-slate-200">
									{event.label}
								</span>
								<span class="font-mono text-[11px] text-slate-500">
									{event.detail}
								</span>
							</div>
						)}
					</For>
				</div>
			</div>
			<div class="rounded-lg border border-kite-cyan/20 bg-kite-cyan/[0.05] px-4 py-3 text-[14px] text-slate-300">
				<span class="font-medium text-slate-100">T2 still sees v1.</span> T1's
				commit stays invisible to T2 for as long as T2 runs; transactions that
				start after the commit see v2.
			</div>
		</Figure>
	);
}

const VERSIONS = [
	{ name: "v3", value: "age=32", commitTs: 150, reader: "T3", startTs: 155 },
	{ name: "v2", value: "age=31", commitTs: 120, reader: "T2", startTs: 125 },
	{ name: "v1", value: "age=30", commitTs: 80, reader: "T1", startTs: 85 },
];

type Version = (typeof VERSIONS)[number];

function VersionCard(props: { version: Version; newest: boolean }) {
	return (
		<div
			class={`rounded-md border px-3 py-2.5 font-mono ${props.newest ? "border-kite-violet/30 bg-kite-violet/[0.07]" : "border-kite-line bg-white/[0.03]"}`}
		>
			<div class="text-[13px] text-slate-100">
				{props.version.name}: {props.version.value}
			</div>
			<div class="mt-0.5 text-[11px] text-slate-500">
				commitTs={props.version.commitTs}
			</div>
		</div>
	);
}

function VersionReader(props: { version: Version }) {
	return (
		<div>
			<div class="text-[12px] text-slate-300">
				<span class="font-mono text-kite-cyan">{props.version.reader}</span>{" "}
				sees this
			</div>
			<div class="font-mono text-[11px] text-slate-500">
				startTs={props.version.startTs}
			</div>
		</div>
	);
}

function VersionChainDiagram() {
	return (
		<Figure
			title='Version chain for node "alice"'
			accent="violet"
			meta="newest first"
		>
			{/* sm and up: chain left to right, readers underneath */}
			<div class="hidden grid-cols-[1fr_auto_1fr_auto_1fr] items-center gap-2 sm:grid">
				<For each={VERSIONS}>
					{(version, i) => (
						<>
							<VersionCard version={version} newest={i() === 0} />
							<Show when={i() < VERSIONS.length - 1}>
								<ArrowLeft
									size={14}
									class="text-slate-600"
									aria-label="previous version"
								/>
							</Show>
						</>
					)}
				</For>
				<For each={VERSIONS}>
					{(version, i) => (
						<>
							<div class="text-center">
								<VersionReader version={version} />
							</div>
							<Show when={i() < VERSIONS.length - 1}>
								<span />
							</Show>
						</>
					)}
				</For>
			</div>
			{/* mobile: one row per version */}
			<div class="space-y-2 sm:hidden">
				<For each={VERSIONS}>
					{(version, i) => (
						<div class="grid grid-cols-[1fr_auto] items-center gap-4">
							<VersionCard version={version} newest={i() === 0} />
							<VersionReader version={version} />
						</div>
					)}
				</For>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-500">
				Each transaction walks the chain from the newest version and reads the
				first one committed before it started.
			</p>
		</Figure>
	);
}

function VisibilityRules() {
	return (
		<Figure title="A version is visible to transaction T if" accent="cyan">
			<div class="space-y-2">
				<div class="flex items-start gap-3 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
					<StepNumber accent="cyan">1</StepNumber>
					<div>
						<code class="font-mono text-[13px] text-slate-100">
							version.commitTs &lt; T.startTs
						</code>
						<p class="mt-1 text-[13px] text-slate-500">
							The version was committed before T started
						</p>
					</div>
				</div>
				<p class="text-center font-mono text-[11px] uppercase tracking-[0.08em] text-slate-500">
					or
				</p>
				<div class="flex items-start gap-3 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
					<StepNumber accent="cyan">2</StepNumber>
					<div>
						<code class="font-mono text-[13px] text-slate-100">
							version.txid == T.txid
						</code>
						<p class="mt-1 text-[13px] text-slate-500">
							T created this version itself (read-your-own-writes)
						</p>
					</div>
				</div>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[14px] text-slate-400">
				Reads walk the chain from newest to oldest and return the first visible
				version. If none is visible, the entity does not exist for this
				transaction.
			</p>
		</Figure>
	);
}

function WriteConflictDiagram() {
	return (
		<Figure title="First-committer-wins" accent="amber">
			<div class="mb-3 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
				<p class="mb-2 text-[13px] text-slate-500">
					Scenario: T1 and T2 both modify "alice"
				</p>
				<div class="space-y-1 text-[14px] text-slate-300">
					<div>
						<span class="mr-2 font-mono text-[12px] text-kite-cyan">T1</span>
						starts at <span class="font-mono text-[13px]">ts=100</span>
					</div>
					<div>
						<span class="mr-2 font-mono text-[12px] text-kite-cyan">T2</span>
						starts at <span class="font-mono text-[13px]">ts=105</span>
					</div>
				</div>
			</div>

			<div class="space-y-2">
				<div class="rounded-lg border border-kite-mint/25 bg-kite-mint/[0.04] px-4 py-3">
					<p class="text-[14px] font-medium text-kite-mint">
						T1 commits first (ts=110)
					</p>
					<p class="mt-0.5 text-[13px] text-slate-400">
						No conflict, so the commit succeeds.
					</p>
				</div>
				<div class="rounded-lg border border-red-400/25 bg-red-400/[0.04] px-4 py-3">
					<p class="text-[14px] font-medium text-red-400">T2 tries to commit</p>
					<p class="mt-0.5 text-[13px] text-slate-400">
						"alice" was modified after T2 started (110 &gt; 105), so T2's commit
						fails with a conflict error and T2 is rolled back.
					</p>
				</div>
			</div>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				Conflicts are checked at commit time against every key the transaction
				read or wrote. To resolve one, retry T2 so it reads the new value.
			</p>
		</Figure>
	);
}

function LazyMVCCDiagram() {
	return (
		<Figure title="Lazy versioning" accent="mint">
			<p class="mb-3 text-[14px] text-slate-300">
				When T1 commits a change to "alice":
			</p>
			<div class="grid gap-2 sm:grid-cols-2">
				<div class="rounded-lg border border-kite-mint/25 bg-kite-mint/[0.04] px-4 py-3">
					<p class="text-[13px] text-slate-500">No other active transactions</p>
					<p class="mt-1 text-[14px] text-slate-200">
						Modify in place, with no version chain
					</p>
				</div>
				<div class="rounded-lg border border-kite-violet/25 bg-kite-violet/[0.04] px-4 py-3">
					<p class="text-[13px] text-slate-500">Other transactions active</p>
					<p class="mt-1 text-[14px] text-slate-200">
						Add a version to the chain and keep the old value
					</p>
				</div>
			</div>
			<p class="mt-4 border-t border-kite-line pt-3 text-[14px] text-slate-400">
				Serial workloads skip version-chain bookkeeping. Concurrent workloads
				still get snapshot isolation.
			</p>
		</Figure>
	);
}

function MVCCGarbageCollection() {
	return (
		<Figure title="Version cleanup" accent="slate">
			<ol class="space-y-2 text-[14px]">
				<li class="flex items-start gap-3">
					<StepNumber accent="cyan">1</StepNumber>
					<span class="text-slate-300">
						Compute the GC horizon: the start of the oldest active transaction,
						or the retention window (<Code>mvccRetentionMs</Code>, default 60
						s), whichever is older
					</span>
				</li>
				<li class="flex items-start gap-3">
					<StepNumber accent="cyan">2</StepNumber>
					<div class="text-slate-300">
						For each version chain:
						<div class="mt-1.5 space-y-1">
							<FlowItem color="emerald">
								Keep versions committed after the horizon, plus the newest one
								committed before it
							</FlowItem>
							<FlowItem color="red">
								Prune older versions; no transaction can see them
							</FlowItem>
						</div>
					</div>
				</li>
			</ol>

			<div class="mt-4 rounded-lg border border-kite-line bg-white/[0.02] px-4 py-3">
				<p class="mb-1.5 text-[13px] text-slate-500">Runs:</p>
				<div class="space-y-1">
					<FlowItem color="slate">When MVCC starts on open</FlowItem>
					<FlowItem color="slate">
						Periodically in a background thread (<Code>mvccGcIntervalMs</Code>,
						default 5 s)
					</FlowItem>
				</div>
			</div>

			<p class="mt-4 border-t border-kite-line pt-3 text-[13px] text-slate-400">
				<span class="text-amber-300">Note:</span> long-running transactions hold
				back the horizon, so old versions stay in memory until they finish.
			</p>
		</Figure>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function MVCCPage() {
	return (
		<DocPage slug="internals/mvcc">
			<p>
				KiteDB supports concurrent transactions using{" "}
				<strong>Multi-Version Concurrency Control (MVCC)</strong>. Multiple
				readers can access the database simultaneously without blocking each
				other or writers.
			</p>
			<p>
				These transactions run inside one process. A writable open takes an
				exclusive lock on the database file, so no other open, in this process
				or another, can use the file until it is closed (see{" "}
				<a href="/docs/internals/single-file#opening">Opening a database</a>).
				v0.2.18 and earlier did not lock the file.
			</p>

			<div class="my-6 rounded-lg border border-kite-cyan/20 bg-kite-cyan/[0.05] px-4 py-3 text-[14px] text-slate-300">
				MVCC is off by default. Enable it with the <code>mvcc: true</code> open
				option; the isolation and conflict behavior on this page applies when it
				is on.
			</div>

			<h2 id="isolation">Snapshot isolation</h2>

			<p>
				Each transaction sees a consistent snapshot of the database as it
				existed when the transaction started. Other transactions' uncommitted
				changes are invisible.
			</p>

			<SnapshotIsolationTimeline />

			<h2 id="version-chains">Version chains</h2>

			<p>
				When data is modified while other transactions are active, KiteDB keeps
				old versions in a chain:
			</p>

			<VersionChainDiagram />

			<p>
				Each chain is keyed by the full IDs of what it versions: the node, the
				edge (source, type, destination), or the property or label together with
				its owner.
			</p>

			<VersionNote>
				edge, property, and label chains were keyed by several IDs packed into
				one 64-bit integer, with as few as 12 bits per ID, so unrelated entries
				could share a chain. For example, two edges whose source node IDs differ
				by 2<sup>20</sup> shared version history.
			</VersionNote>

			<h2 id="visibility">Visibility rules</h2>

			<VisibilityRules />

			<h2 id="conflict-detection">Write conflicts</h2>

			<p>
				KiteDB uses <strong>first-committer-wins</strong> to handle conflicts:
			</p>

			<WriteConflictDiagram />

			<CodeBlock
				code={`// Handling conflicts
try {
  db.transaction((ctx) => {
    const alice = ctx.get(user, 'alice');
    if (!alice) return;
    const update = ctx.update(user, 'alice');
    update.setAll({ age: alice.age + 1 });
    update.execute();
  });
} catch (e) {
  // The commit failed, for example because another
  // transaction modified alice first. Nothing was applied;
  // retry with fresh data.
}`}
				language="typescript"
			/>

			<h2 id="lazy-versioning">Lazy version chains</h2>

			<p>
				Version chains are only created when necessary. If no other transactions
				are active, modifications happen in place without versioning overhead.
			</p>

			<LazyMVCCDiagram />

			<h2 id="garbage-collection">Garbage collection</h2>

			<p>Old versions are cleaned up when no transaction can see them:</p>

			<MVCCGarbageCollection />

			<h2 id="transaction-api">Transaction API</h2>

			<CodeBlock
				code={`// Explicit transaction (the API is synchronous)
db.transaction((ctx) => {
  const alice = ctx.get(user, 'alice');
  if (!alice) return;
  const update = ctx.update(user, 'alice');
  update.setAll({ age: alice.age + 1 });
  update.execute();
  // Commits when the callback returns
  // Rolls back if it throws
});

// Batch operations: batch() executes each builder
// inside a single transaction
db.batch([
  db.insert(user).values({ key: 'bob', name: 'Bob' }),
  db.insert(user).values({ key: 'carol', name: 'Carol' }),
]);

// Without an explicit transaction, each operation commits on its own`}
				language="typescript"
			/>

			<h2 id="next">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/wal">WAL and durability</a>: how commits are
					made durable
				</li>
				<li>
					<a href="/docs/guides/transactions">Transactions</a>: practical usage
					patterns
				</li>
				<li>
					<a href="/docs/guides/concurrency">Concurrency</a>: multi-threaded
					access
				</li>
			</ul>
		</DocPage>
	);
}

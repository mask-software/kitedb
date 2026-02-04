import CodeBlock from "~/components/code-block";
import DocPage from "~/components/doc-page";

// ============================================================================
// PERFORMANCE-SPECIFIC COMPONENTS
// ============================================================================

// Network overhead comparison
function NetworkOverheadComparison() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Traditional */}
			<div class="rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-red-400 mb-3">
					Traditional Database
				</div>
				<div class="flex items-center gap-1 text-xs mb-3 flex-wrap">
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-red-500/20 text-red-400">
						Network
					</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">DB</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">
						Disk
					</span>
					<span class="text-slate-500">→</span>
					<span class="text-slate-400">...</span>
				</div>
				<div class="text-sm">
					<span class="text-slate-400">Latency:</span>
					<span class="text-red-400 font-mono ml-2">1-10ms</span>
					<span class="text-slate-500 text-xs ml-1">per operation</span>
				</div>
			</div>

			{/* KiteDB */}
			<div class="rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-emerald-400 mb-3">
					KiteDB (embedded)
				</div>
				<div class="flex items-center gap-1 text-xs mb-3">
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-emerald-500/20 text-emerald-400">
						Memory/Disk
					</span>
					<span class="text-slate-500">→</span>
					<span class="px-2 py-1 rounded bg-slate-700 text-slate-300">App</span>
				</div>
				<div class="text-sm">
					<span class="text-slate-400">Latency:</span>
					<span class="text-emerald-400 font-mono ml-2">1-100μs</span>
					<span class="text-slate-500 text-xs ml-1">per operation</span>
				</div>
			</div>
		</div>
	);
}

// Zero-copy mmap comparison
function ZeroCopyComparison() {
	return (
		<div class="my-6 grid sm:grid-cols-2 gap-4">
			{/* Traditional */}
			<div class="rounded-xl border border-red-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-red-400 mb-3">
					Traditional Read
				</div>
				<div class="space-y-1 text-xs mb-3">
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400">Disk</span>
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">Kernel buffer</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">User buffer</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-slate-300">Parse → Use</span>
					</div>
				</div>
				<div class="text-sm text-red-400">
					2+ memory copies, allocation overhead
				</div>
			</div>

			{/* KiteDB */}
			<div class="rounded-xl border border-emerald-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-4 shadow-lg">
				<div class="text-sm font-medium text-emerald-400 mb-3">
					KiteDB (mmap)
				</div>
				<div class="space-y-1 text-xs mb-3">
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400">Disk</span>
						<span class="text-slate-500">→</span>
						<span class="text-emerald-300">Page cache</span>
					</div>
					<div class="flex items-center gap-2">
						<span class="w-24 text-slate-400" />
						<span class="text-slate-500">→</span>
						<span class="text-emerald-300">Direct access</span>
					</div>
				</div>
				<div class="text-sm text-emerald-400">
					0 copies — OS handles caching
				</div>
			</div>
		</div>
	);
}

// Cache-friendly layout comparison
function CacheFriendlyComparison() {
	return (
		<div class="my-6 rounded-xl border border-cyan-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-cyan-400 mb-4">Traversing 10 Neighbors</h4>

			<div class="space-y-3">
				{/* Linked list */}
				<div class="flex items-center gap-4 p-3 rounded-lg bg-red-500/10 border border-red-500/20">
					<div class="w-24 text-sm font-medium text-red-400">Linked List</div>
					<div class="flex-1 text-sm text-slate-400">
						10 random accesses × <span class="font-mono">100ns</span>
					</div>
					<div class="text-red-400 font-mono font-medium">= 1000ns</div>
				</div>

				{/* CSR */}
				<div class="flex items-center gap-4 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<div class="w-24 text-sm font-medium text-emerald-400">CSR</div>
					<div class="flex-1 text-sm text-slate-400">
						1 seq read × <span class="font-mono">10ns</span> + 10 cache hits ×{" "}
						<span class="font-mono">1ns</span>
					</div>
					<div class="text-emerald-400 font-mono font-medium">= 20ns</div>
				</div>
			</div>

			<div class="mt-4 pt-3 border-t border-slate-700/50 text-center">
				<span class="text-emerald-400 font-bold text-lg">50x</span>
				<span class="text-slate-400 text-sm ml-2">
					speedup for traversal operations
				</span>
			</div>
		</div>
	);
}

// Lazy MVCC comparison
function LazyMVCCComparison() {
	return (
		<div class="my-6 rounded-xl border border-violet-500/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-violet-400 mb-4">
				Version Chains: Only When Needed
			</h4>

			<div class="space-y-3">
				{/* Serial */}
				<div class="p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
					<div class="flex items-center justify-between mb-1">
						<span class="text-sm font-medium text-emerald-400">
							Serial workload
						</span>
						<span class="text-xs text-slate-500">(no concurrent readers)</span>
					</div>
					<div class="flex items-center justify-between text-sm">
						<span class="text-slate-400">Modify → Update in-place</span>
						<span class="text-emerald-400 font-mono">Overhead: 0</span>
					</div>
				</div>

				{/* Concurrent */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="flex items-center justify-between mb-1">
						<span class="text-sm font-medium text-violet-400">
							Concurrent workload
						</span>
						<span class="text-xs text-slate-500">(active readers)</span>
					</div>
					<div class="flex items-center justify-between text-sm">
						<span class="text-slate-400">Modify → Create version chain</span>
						<span class="text-violet-400 font-mono">∝ concurrency</span>
					</div>
				</div>
			</div>

			<p class="text-xs text-slate-500 mt-4 pt-3 border-t border-slate-700/50">
				Most workloads are mostly serial. MVCC overhead is paid only when
				required.
			</p>
		</div>
	);
}

// Memory usage breakdown
function MemoryUsageBreakdown() {
	return (
		<div class="my-6 rounded-xl border border-slate-600/30 bg-gradient-to-br from-slate-900 to-slate-800 p-5 shadow-lg">
			<h4 class="font-semibold text-slate-400 mb-4">Memory Breakdown</h4>

			<div class="space-y-3">
				{/* Snapshot */}
				<div class="p-3 rounded-lg bg-cyan-500/10 border border-cyan-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-cyan-400">
							1. Snapshot (mmap'd)
						</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-400 ml-4">
						<li>• Not counted against process memory</li>
						<li>• OS manages page cache</li>
						<li>• Hot pages in RAM, cold pages on disk</li>
					</ul>
				</div>

				{/* Delta */}
				<div class="p-3 rounded-lg bg-violet-500/10 border border-violet-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-violet-400">2. Delta</span>
					</div>
					<div class="grid grid-cols-2 gap-2 text-xs ml-4">
						<div class="flex justify-between">
							<span class="text-slate-400">Created nodes:</span>
							<span class="text-slate-300 font-mono">~200 B/node</span>
						</div>
						<div class="flex justify-between">
							<span class="text-slate-400">Modified nodes:</span>
							<span class="text-slate-300 font-mono">~100 B/change</span>
						</div>
						<div class="flex justify-between">
							<span class="text-slate-400">Edges:</span>
							<span class="text-slate-300 font-mono">~20 B/edge</span>
						</div>
					</div>
				</div>

				{/* Caches */}
				<div class="p-3 rounded-lg bg-amber-500/10 border border-amber-500/20">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-amber-400">3. Caches</span>
						<span class="text-xs text-slate-500">(configurable)</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-400 ml-4">
						<li>• Property cache: LRU, default 10K entries</li>
						<li>• Traversal cache: LRU, invalidated on writes</li>
					</ul>
				</div>

				{/* MVCC */}
				<div class="p-3 rounded-lg bg-slate-700/30 border border-slate-600/30">
					<div class="flex items-center justify-between mb-2">
						<span class="text-sm font-medium text-slate-400">
							4. MVCC version chains
						</span>
					</div>
					<ul class="space-y-1 text-xs text-slate-500 ml-4">
						<li>• Only when concurrent transactions exist</li>
						<li>• Cleaned up by GC</li>
					</ul>
				</div>
			</div>

			{/* Example */}
			<div class="mt-4 pt-4 border-t border-slate-700/50 p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20">
				<p class="text-xs text-slate-500 mb-2">Typical 100K node graph:</p>
				<div class="flex justify-between text-sm">
					<span class="text-slate-400">Snapshot on disk:</span>
					<span class="text-slate-300 font-mono">~10MB (compressed)</span>
				</div>
				<div class="flex justify-between text-sm">
					<span class="text-slate-400">Memory footprint:</span>
					<span class="text-emerald-400 font-mono font-medium">~5MB</span>
				</div>
			</div>
		</div>
	);
}

// ============================================================================
// PAGE COMPONENT
// ============================================================================

export function PerformancePage() {
	return (
		<DocPage slug="internals/performance">
			<p>
				KiteDB is designed for speed. This page explains why it's fast and how
				to get the best performance from it.
			</p>

			<h2 id="why-fast">Why KiteDB is Fast</h2>

			<h3>1. No Network Overhead</h3>
			<NetworkOverheadComparison />
			<p class="text-sm text-slate-400 mb-6">
				<span class="text-emerald-400 font-bold">10-1000x</span> speedup just
				from eliminating network.
			</p>

			<h3>2. Zero-Copy Memory Mapping</h3>
			<ZeroCopyComparison />
			<p class="text-sm text-slate-400 mb-6">
				Hot data stays in RAM automatically. Cold data is paged in on demand.
			</p>

			<h3>3. Cache-Friendly Data Layout</h3>
			<CacheFriendlyComparison />

			<h3>4. Lazy MVCC</h3>
			<LazyMVCCComparison />

			<h2 id="benchmarks">Benchmark Results</h2>

			<p>
				Latest snapshot (single-file raw, Rust core, 10k nodes / 50k edges,
				edge types=3, edge props=10, sync_mode=Normal, group_commit=false,
				February 4, 2026):
			</p>

			<h3>Node Ops</h3>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>p50</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Key lookup (random existing)</td>
						<td>125ns</td>
					</tr>
					<tr>
						<td>Batch write (100 nodes)</td>
						<td>34.08us</td>
					</tr>
				</tbody>
			</table>

			<h3>Edge Ops</h3>
			<table>
				<thead>
					<tr>
						<th>Operation</th>
						<th>p50</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>1-hop traversal (out)</td>
						<td>208ns</td>
					</tr>
					<tr>
						<td>Edge exists (random)</td>
						<td>83ns</td>
					</tr>
					<tr>
						<td>Batch write (100 edges)</td>
						<td>40.25us</td>
					</tr>
					<tr>
						<td>Batch write (100 edges + props)</td>
						<td>172.33us</td>
					</tr>
				</tbody>
			</table>

			<p>
				Full logs and run commands are in <code>docs/benchmarks/results/</code>.
			</p>

			<h3>Write Durability vs Throughput</h3>
			<ul class="space-y-2 text-sm text-slate-400">
				<li>
					<b class="text-slate-200">Defaults stay safe:</b> <code>sync_mode=Full</code>,{" "}
					<code>group_commit=false</code>.
				</li>
				<li>
					<b class="text-slate-200">Single-writer, low latency:</b>{" "}
					<code>sync_mode=Normal</code> + <code>group_commit=false</code>.
				</li>
				<li>
					<b class="text-slate-200">Multi-writer throughput:</b>{" "}
					<code>sync_mode=Normal</code> + <code>group_commit=true</code> (1-2ms).
				</li>
				<li>
					<b class="text-slate-200">Highest speed, weakest durability:</b>{" "}
					<code>sync_mode=Off</code> (testing/throwaway only).
				</li>
			</ul>
			<p class="text-sm text-slate-500 mt-3">
				Group commit adds intentional latency to coalesce commits; it improves
				throughput under concurrency, but can slow single-threaded benchmarks.
			</p>

			<h4 class="mt-6">Decision Table</h4>
			<table>
				<thead>
					<tr>
						<th>Workload</th>
						<th>sync_mode</th>
						<th>group_commit</th>
						<th>Why</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>Production, high durability</td>
						<td>Full</td>
						<td>Off</td>
						<td>fsync per commit</td>
					</tr>
					<tr>
						<td>Single-writer ingest</td>
						<td>Normal</td>
						<td>Off</td>
						<td>Lowest latency per commit</td>
					</tr>
					<tr>
						<td>Multi-writer throughput</td>
						<td>Normal</td>
						<td>On (1-2ms)</td>
						<td>Coalesces commits</td>
					</tr>
					<tr>
						<td>Testing/throwaway data</td>
						<td>Off</td>
						<td>Off</td>
						<td>Max speed, weakest durability</td>
					</tr>
				</tbody>
			</table>

			<h2 id="playbook">Performance Playbook</h2>
			<ul class="space-y-2 text-sm text-slate-400">
				<li>
					<b class="text-slate-200">Fastest ingest (single writer):</b>{" "}
					<code>begin_bulk()</code> + <code>create_nodes_batch()</code> +{" "}
					<code>add_edges_batch()</code> / <code>add_edges_with_props_batch()</code>,{" "}
					<code>sync_mode=Normal</code>, <code>group_commit=false</code>, WAL ≥ 256MB,
					auto-checkpoint off during ingest, then checkpoint.
				</li>
				<li>
					<b class="text-slate-200">Multi-writer throughput:</b>{" "}
					<code>sync_mode=Normal</code> + <code>group_commit=true</code> (1-2ms window),
					batched ops per transaction.
				</li>
				<li>
					<b class="text-slate-200">Read-heavy, mixed workload:</b>{" "}
					Keep batches small, checkpoint when WAL ≥ 80%, avoid deep traversals.
				</li>
				<li>
					<b class="text-slate-200">Max speed, lowest durability:</b>{" "}
					<code>sync_mode=Off</code> for testing only.
				</li>
			</ul>
			<p class="text-sm text-slate-500 mt-3">
				Bulk-load mode requires MVCC disabled. Use it for one-shot ingest or ETL jobs.
			</p>

			<h3 class="mt-6">Bulk Ingest Example (Low-Level)</h3>
			<CodeBlock
				code={`// Fast ingest: low-level API
db.begin_bulk();
const nodeIds = db.create_nodes_batch(keys); // keys: string[]
db.add_edges_batch(edges); // edges: { src, etype, dst }[]
db.add_edges_with_props_batch(edgesWithProps);
db.commit();

// Optional: checkpoint after ingest
db.checkpoint();`}
				language="typescript"
			/>

			<h2 id="best-practices">Best Practices</h2>

			<h3>Batch Writes</h3>
			<CodeBlock
				code={`// Slow: Individual inserts (1 WAL sync per op)
for (const key of keys) {
  db.create_node(key);
}

// Fast: Batch + bulk-load transaction
db.begin_bulk();
db.create_nodes_batch(keys);
db.commit();

// 1000 nodes × 1μs + 1 WAL sync = ~2ms`}
				language="typescript"
			/>

			<h3>Limit Traversal Depth</h3>
			<CodeBlock
				code={`// Potentially expensive: deep traversal
const alice = await db.get(user, 'alice');
const all = db
  .from(alice)
  .traverse(follows, { direction: 'out', maxDepth: 10 })
  .nodes()
  .toArray();

// Safer: bounded traversal + limit
const friends = db
  .from(alice)
  .traverse(follows, { direction: 'out', maxDepth: 2 })
  .take(100)
  .nodes()
  .toArray();`}
				language="typescript"
			/>

			<h3>Use Keys for Lookups</h3>
			<CodeBlock
				code={`// Fast: Key lookup (O(1) hash index)
const alice = await db.get(user, 'alice');

// Slower: Property scan (O(n) nodes, done in JS)
const aliceByName = db.all(user).find((u) => u.name === 'Alice');

Design keys to match your access patterns.`}
				language="typescript"
			/>

			<h3>Checkpoint Timing</h3>
			<CodeBlock
				code={`// For write-heavy bursts: Compact snapshots after large ingests
await importLargeDataset();
await db.optimize();

// Inspect storage stats
const stats = await db.stats();`}
				language="typescript"
			/>

			<h2 id="memory">Memory Usage</h2>

			<MemoryUsageBreakdown />

			<h2 id="profiling">Profiling Tips</h2>

			<CodeBlock
				code={`// Get database statistics
const stats = await db.stats();
console.log(stats);
// {
//   nodes: 100000,
//   edges: 500000,
//   snapshotSize: 10485760,
//   deltaSize: 524288,
//   walUsage: 0.45
// }

// If walUsage is consistently high:
// → Checkpoint more frequently or increase WAL size

// If deltaSize is large:
// → Checkpoint to consolidate into snapshot`}
				language="typescript"
			/>

			<h2 id="next">Next Steps</h2>
			<ul>
				<li>
					<a href="/docs/internals/csr">CSR Format</a> – Why traversals are fast
				</li>
				<li>
					<a href="/docs/internals/snapshot-delta">Snapshot + Delta</a> – How
					reads stay fast during writes
				</li>
				<li>
					<a href="/docs/benchmarks">Benchmarks</a> – Detailed performance
					measurements
				</li>
			</ul>
		</DocPage>
	);
}

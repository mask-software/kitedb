import { createFileRoute } from "@tanstack/solid-router";
import { Match, Switch } from "solid-js";
import CodeBlock from "~/components/code-block";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { loadDocSlug } from "~/lib/doc-route";

export const Route = createFileRoute("/docs/api/$")({
	loader: loadDocSlug,
	component: ApiSplatPage,
	notFoundComponent: () => <DocNotFound />,
});

function ApiSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

function HighLevelPage() {
	return (
		<DocPage slug="api/high-level">
			<p>
				The high-level API wraps the database in a <code>Kite</code> instance
				with a Drizzle-style fluent interface. Methods take the node and edge
				definitions from your schema.
			</p>

			<h2 id="kite-function">kite()</h2>
			<p>
				Open a database. <code>kite()</code> returns a promise;{" "}
				<code>kiteSync()</code> is the synchronous version.
			</p>
			<CodeBlock
				code={`import { kite } from '@kitedb/core';

const db = await kite(path, options);`}
				language="typescript"
			/>

			<h3 id="mvcc-options">MVCC options</h3>
			<p>
				<code>kite()</code>, <code>kiteSync()</code>, and{" "}
				<code>Database.open()</code> take the same MVCC options:
			</p>
			<table>
				<thead>
					<tr>
						<th>Option</th>
						<th>Default</th>
						<th>Description</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>
							<code>mvcc</code>
						</td>
						<td>
							<code>true</code>
						</td>
						<td>
							Snapshot-isolated transactions; concurrent write transactions with
							conflict detection at commit
						</td>
					</tr>
					<tr>
						<td>
							<code>mvccGcIntervalMs</code>
						</td>
						<td>5000</td>
						<td>Milliseconds between background version-history cleanups</td>
					</tr>
					<tr>
						<td>
							<code>mvccRetentionMs</code>
						</td>
						<td>0</td>
						<td>
							How long to keep version history beyond what open transactions
							need
						</td>
					</tr>
					<tr>
						<td>
							<code>mvccMaxChainDepth</code>
						</td>
						<td>10</td>
						<td>Version chain depth that cleanup truncates to</td>
					</tr>
				</tbody>
			</table>
			<p>
				<code>mvcc: false</code> is deprecated and will be removed in a later
				release; it logs no warning. Without MVCC, write transactions run one at
				a time and transactions read the latest committed state instead of a
				snapshot. The file format is the same in both modes. See{" "}
				<a href="/docs/internals/mvcc">MVCC and transactions</a>.
			</p>

			<h2 id="node-methods">Node methods</h2>
			<CodeBlock
				code={`// Create nodes
db.insert(user).values({ key: "alice", name: "Alice" }).returning()
db.insert(user).valuesMany([{ key: "a" }, { key: "b" }]).execute()

// Upsert by key
db.upsert(user).values({ key: "alice", email: "a@x.com" }).execute()

// Read
db.get(user, "alice")
db.getRef(user, "alice")

// Update by key (setAll returns void, so call execute() separately)
const update = db.update(user, "alice")
update.setAll({ name: "Alice V2" })
update.execute()

// Delete by key
db.delete(user, "alice")

// List / count
db.all(user)
db.countNodes()
db.countNodes(user)`}
				language="typescript"
			/>

			<h2 id="edge-methods">Edge methods</h2>
			<CodeBlock
				code={`// Create edge
db.link(src, follows, dst, { since: 2024 })
db.link(src).to(dst).via(follows).props({ since: 2024 }).execute()

// Delete / check
db.unlink(src, follows, dst)
db.hasEdge(src, follows, dst)

// Update edge props
const edgeUpdate = db.updateEdge(src, follows, dst)
edgeUpdate.setAll({ weight: 0.8 })
edgeUpdate.execute()

// List / count
db.allEdges()
db.allEdges(follows)
db.countEdges()
db.countEdges(follows)`}
				language="typescript"
			/>

			<h2 id="next-steps">Next steps</h2>
			<ul>
				<li>
					<a href="/docs/api/low-level">Low-level API</a>: direct database
					primitives
				</li>
				<li>
					<a href="/docs/api/vector-api">Vector API</a>: similarity search
				</li>
			</ul>
		</DocPage>
	);
}

function LowLevelPage() {
	return (
		<DocPage slug="api/low-level">
			<p>
				The low-level API uses the <code>Database</code> class for direct graph
				operations, transaction control, and batched writes.
			</p>

			<h2 id="storage-access">Open and write</h2>
			<CodeBlock
				code={`import { Database, PropValueType } from '@kitedb/core';

const db = Database.open('./data.kitedb', { createIfMissing: true });

db.begin();
try {
  const nodeId = db.createNode('user:alice');
  db.setNodePropByName(nodeId, 'name', {
    propType: PropValueType.String,
    stringValue: 'Alice',
  });

  db.commit();
} catch (err) {
  db.rollback();
  throw err;
}`}
				language="typescript"
			/>

			<p>
				<code>Database.open()</code> takes the{" "}
				<a href="/docs/api/high-level#mvcc-options">MVCC options</a>; MVCC is on
				by default and <code>mvcc: false</code> is deprecated. A commit that
				conflicts with a write committed since its transaction began throws an{" "}
				<code>Error</code> whose message reads{" "}
				<code>
					Failed to commit: Transaction &lt;id&gt; conflict on keys: [...]
				</code>
				. The transaction has then ended and nothing was applied; run it again.
			</p>

			<h2 id="batch-operations">Batch operations</h2>
			<p>
				<code>beginBulk()</code> starts a bulk-load transaction, the fastest way
				to load data, with MVCC on or off. It runs alone among writers: it waits
				for open write transactions to finish, and write transactions that begin
				while it is open wait for it. Readers never wait for it, and a read
				transaction that began before its commit does not see it.
			</p>
			<CodeBlock
				code={`// High-throughput bulk ingest
db.beginBulk();
const nodeIds = db.createNodesBatch(keys); // Array<string | null>
db.addEdgesBatch(edges);                   // Array<{ src, etype, dst }>
db.addEdgesWithPropsBatch(edgesWithProps);
db.commit();

// Optional maintenance checkpoint after ingest
db.checkpoint();`}
				language="typescript"
			/>
			<p>
				<code>bulkWrite()</code> runs a list of synchronous operations in
				bulk-load transactions of <code>chunkSize</code> operations each
				(default 1000), committing after each chunk.
			</p>
			<CodeBlock
				code={`import { bulkWrite } from '@kitedb/core';

const nodeIds = bulkWrite(
  db,
  keys.map((key) => (d) => d.createNode(key)),
  { chunkSize: 5000 },
);`}
				language="typescript"
			/>

			<h2 id="checkpoints">Checkpoints</h2>
			<p>
				A checkpoint folds the log (the WAL, and the WAL segments a full WAL
				spills into) into a new snapshot. Automatic checkpoints run on a thread
				of the database's own; see{" "}
				<a href="/docs/internals/wal#checkpoint-trigger">
					When checkpoints happen
				</a>
				. <code>Database.open()</code> takes these options. <code>kite()</code>{" "}
				takes the same ones except <code>autoCheckpoint</code> and{" "}
				<code>backgroundCheckpoint</code>, with <code>walSizeMb</code> in place
				of <code>walSize</code>.
			</p>
			<table>
				<thead>
					<tr>
						<th>Option</th>
						<th>Default</th>
						<th>Description</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>
							<code>walSize</code>
						</td>
						<td>4 MB</td>
						<td>
							Bytes of the WAL area, fixed when the file is created. A full WAL
							spills into a WAL segment. Besides how often it spills, it sets
							the floors of the checkpoint trigger (3/8 of the WAL), the segment
							limit (16 WALs) and the segment extent (2 WALs)
						</td>
					</tr>
					<tr>
						<td>
							<code>autoCheckpoint</code>
						</td>
						<td>
							<code>true</code>
						</td>
						<td>
							Checkpoint automatically. Without it, the WAL spills into segments
							up to <code>walSegmentLimit</code>, then writes fail until a
							checkpoint
						</td>
					</tr>
					<tr>
						<td>
							<code>backgroundCheckpoint</code>
						</td>
						<td>
							<code>true</code>
						</td>
						<td>
							Automatic checkpoints run while writes continue;{" "}
							<code>false</code> makes them blocking (they run after a commit,
							and a writer at <code>walSegmentLimit</code> fails instead of
							waiting; one runs once its transaction ends)
						</td>
					</tr>
					<tr>
						<td>
							<code>checkpointThread</code>
						</td>
						<td>
							<code>true</code>
						</td>
						<td>
							Run automatic background checkpoints on the database's checkpoint
							thread, so the commit that crosses the trigger returns at once;{" "}
							<code>false</code> runs them on the committing thread
						</td>
					</tr>
					<tr>
						<td>
							<code>checkpointLogRatio</code>
						</td>
						<td>0.5</td>
						<td>
							Checkpoint once the log the snapshot does not cover reaches this
							fraction of the snapshot's size (at least 3/8 of the WAL, where
							earlier releases checkpointed; at most{" "}
							<code>checkpointLogBudget</code>)
						</td>
					</tr>
					<tr>
						<td>
							<code>checkpointLogBudget</code>
						</td>
						<td>128 MiB</td>
						<td>
							The most log, in bytes, an automatic checkpoint waits for. The
							in-memory delta takes about ten times the log's size, so this
							bounds its memory while checkpoints keep up (writers that outrun
							them grow the log up to <code>walSegmentLimit</code>)
						</td>
					</tr>
					<tr>
						<td>
							<code>walSegmentSize</code>
						</td>
						<td>
							1/16 of the segment limit, from 2 WALs to max(32 MiB, 2 WALs)
						</td>
						<td>
							Bytes of a WAL segment extent (never less than 1.5 WALs). An open
							transaction keeps the extent it began in whole until a checkpoint
							covers its commit, so extents stay small next to the limit
						</td>
					</tr>
					<tr>
						<td>
							<code>walSegmentLimit</code>
						</td>
						<td>
							Twice the trigger, at least 16 WALs, at most four times{" "}
							<code>checkpointLogBudget</code>
						</td>
						<td>
							The most bytes of WAL segments. At the limit, writers wait for a
							checkpoint to free some; before it, while a background checkpoint
							runs past the trigger, each commit waits up to 100 ms so the room
							left lasts the run (and once per run, a commit may wait for the
							install, for a time that grows with the delta). The segment table also caps them at 63
							extents: with default extents, about four times a limit up to 512
							MiB, and 2 GiB beyond; with <code>walSegmentSize</code> set, 63
							extents of it
						</td>
					</tr>
					<tr>
						<td>
							<code>checkpointThreshold</code>
						</td>
						<td>none</td>
						<td>
							Deprecated, no effect. Still accepted (0 to 1); use{" "}
							<code>checkpointLogRatio</code> and{" "}
							<code>checkpointLogBudget</code>
						</td>
					</tr>
				</tbody>
			</table>
			<CodeBlock
				code={`db.checkpoint();           // blocking: waits for open transactions, leaves no WAL segments
db.backgroundCheckpoint(); // writes continue; runs on this thread, returns after its install
db.shouldCheckpoint(0.8);  // has the uncovered log reached 0.8 of the trigger? (default 0.8)
db.checkpointError();      // the last automatic checkpoint's error, or null
db.close();                // a clean close checkpoints any WAL segments away`}
				language="typescript"
			/>
			<p>
				A failed automatic checkpoint, on the checkpoint thread or inline,
				reports nothing to the commit that started it. Its error is logged and
				returned by <code>checkpointError()</code> (also on <code>Kite</code>;{" "}
				<code>checkpoint_error()</code> on a Python <code>Database</code>) until
				a checkpoint succeeds, and the next automatic checkpoint waits out a
				back-off (1 s, doubling to 60 s while failures go on; any checkpoint
				that succeeds ends it). While the last one failed, a writer that finds
				the WAL segments full fails with{" "}
				<code>Checkpoint failed, and the WAL segments are full: ...</code> (
				<code>CheckpointError</code> in Python) instead of waiting.{" "}
				<code>backgroundCheckpoint()</code> declines (
				<code>Background checkpoint declined: ...</code>;{" "}
				<code>CheckpointDeclinedError</code> in Python) while a blocking
				checkpoint or compaction waits for the gate, or when open write
				transactions hold every WAL segment.
			</p>

			<h2 id="async-maintenance">Long-running calls</h2>
			<p>
				Calls that can take a long time have <code>*Async</code> variants that
				run on the libuv thread pool and return a Promise, so the event loop
				keeps serving other work: <code>checkpointAsync()</code>,{" "}
				<code>optimizeAsync()</code>, <code>vacuumAsync()</code>,{" "}
				<code>exportToJsonAsync()</code>, <code>exportToJsonlAsync()</code>,{" "}
				<code>importFromJsonAsync()</code>, <code>waitForTokenAsync()</code>,
				and the functions <code>createBackupAsync()</code>,{" "}
				<code>restoreBackupAsync()</code>,{" "}
				<code>createOfflineBackupAsync()</code> and the{" "}
				<code>pushReplicationMetricsOtel*Async()</code> pushes. They reject
				inside a transaction (commit or roll back first), and{" "}
				<code>close()</code> fails while one is still running.
			</p>
			<CodeBlock
				code={`await db.checkpointAsync();
const reached = await db.waitForTokenAsync(token, 5_000);`}
				language="typescript"
			/>

			<h2 id="iterators">Streaming and pagination</h2>
			<p>
				<code>streamNodes()</code> returns all node IDs split into batches.{" "}
				<code>get_nodes_page()</code> returns one page of node IDs and a cursor
				for the next page. It is snake_case in the TypeScript bindings.
			</p>
			<CodeBlock
				code={`// Node IDs in fixed-size batches
for (const batch of db.streamNodes({ batchSize: 1000 })) {
  for (const nodeId of batch) {
    // process nodeId
  }
}

// Cursor pagination
let cursor: string | undefined = undefined;
do {
  const page = db.get_nodes_page({ limit: 100, cursor });
  for (const nodeId of page.items) {
    // process nodeId
  }
  cursor = page.nextCursor;
} while (cursor);`}
				language="typescript"
			/>
		</DocPage>
	);
}

function VectorApiPage() {
	return (
		<DocPage slug="api/vector-api">
			<p>
				Declare vector properties in your schema, and use a standalone vector
				index for similarity search.
			</p>

			<h2 id="vector-property">Defining vector properties</h2>
			<p>The dimension argument documents the property; it is not validated.</p>
			<CodeBlock
				code={`import { vector } from '@kitedb/core';

// Define with dimensions
embedding: vector('embedding', 1536)`}
				language="typescript"
			/>

			<h2 id="similarity-methods">Similarity search methods</h2>
			<CodeBlock
				code={`import { createVectorIndex } from '@kitedb/core';

const index = createVectorIndex({ dimensions: 1536 });

// Add vectors: a Float32Array is read in place, a number[] is converted
index.set(nodeId, embedding);

// Search: returns Array<{ nodeId, distance, similarity }>
const hits = index.search(queryVector, {
  k: 10,          // Max results
  threshold: 0.8, // Min similarity score (cosine)
  nProbe: 10,     // IVF probe count (optional)
  rerankFactor: 4, // IVF-PQ: re-rank the best max(k * 4, 80) candidates
                   // by exact distance (optional; 0 = PQ ranking only)
});`}
				language="typescript"
			/>

			<h2 id="indexing">Vector indexing</h2>
			<CodeBlock
				code={`import { AnnAlgorithm, createVectorIndex } from '@kitedb/core';

const index = createVectorIndex({
  dimensions: 1536,
  // ivf.seed (optional) makes builds reproducible: the same vectors give
  // the same index on any machine
  ivf: { seed: 42 },
  // AnnAlgorithm.Auto (default): plain IVF below 50,000 vectors or 512
  // dimensions, IVF-PQ from there on; or force AnnAlgorithm.Ivf / IvfPq
  annAlgorithm: AnnAlgorithm.Auto,
});

// Build or rebuild the ANN index for faster search
index.buildIndex();
index.stats().indexAlgorithm; // 'ivf' or 'ivf_pq'

// Or build on the libuv thread pool without blocking the event loop
await index.buildIndexAsync();`}
				language="typescript"
			/>
		</DocPage>
	);
}

function DocPageContent(props: { slug: string }) {
	return (
		<Switch
			fallback={
				<DocPage slug={props.slug}>
					<p>This API reference is coming soon.</p>
				</DocPage>
			}
		>
			<Match when={props.slug === "api/high-level"}>
				<HighLevelPage />
			</Match>
			<Match when={props.slug === "api/low-level"}>
				<LowLevelPage />
			</Match>
			<Match when={props.slug === "api/vector-api"}>
				<VectorApiPage />
			</Match>
		</Switch>
	);
}

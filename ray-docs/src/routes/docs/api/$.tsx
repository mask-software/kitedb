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

			<h2 id="batch-operations">Batch operations</h2>
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

// Add vectors
index.set(nodeId, embedding);

// Search: returns Array<{ nodeId, distance, similarity }>
const hits = index.search(queryVector, {
  k: 10,          // Max results
  threshold: 0.8, // Min similarity score (cosine)
  nProbe: 10,     // IVF probe count (optional)
});`}
				language="typescript"
			/>

			<h2 id="indexing">Vector indexing</h2>
			<CodeBlock
				code={`const index = createVectorIndex({ dimensions: 1536 });

// Build or rebuild the ANN index (IVF-PQ by default) for faster search
index.buildIndex();`}
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

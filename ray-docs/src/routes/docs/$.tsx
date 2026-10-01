import { createFileRoute } from "@tanstack/solid-router";
import CodeBlock from "~/components/code-block";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { loadDocSlug } from "~/lib/doc-route";

export const Route = createFileRoute("/docs/$")({
	loader: loadDocSlug,
	component: DocSplatPage,
	notFoundComponent: () => <DocNotFound />,
});

function DocSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

function DocPageContent(props: { slug: string }) {
	const slug = props.slug;

	// Introduction page (empty slug)
	if (slug === "") {
		return (
			<DocPage slug="">
				<p>
					KiteDB is an embedded graph database with built-in vector search. It
					runs inside your application process and keeps nodes, edges,
					properties, and embeddings in a single file. Bindings are available
					for TypeScript, Python, and Rust.
				</p>

				<h2 id="what-is-kitedb">When to use it</h2>
				<p>
					KiteDB fits applications whose data is mostly relationships, such as
					users and follows, documents and citations, or code symbols and their
					references, and that also need similarity search over embeddings.
					Vectors are keyed by node ID, so a vector search returns nodes you can
					traverse from directly.
				</p>
				<p>
					Because it is embedded, there is no server to deploy or connect to.
					You open a file path and get a database handle, the same way you would
					use SQLite.
				</p>

				<h2 id="key-features">Key features</h2>
				<ul>
					<li>
						<strong>Graph-native</strong>: nodes, edges, and properties are
						first-class, and traversals chain across multiple hops
					</li>
					<li>
						<strong>Vector search</strong>: IVF-based approximate
						nearest-neighbor queries over embeddings
					</li>
					<li>
						<strong>Embedded</strong>: runs in your process; the database is a
						single <code>.kitedb</code> file
					</li>
					<li>
						<strong>Typed schemas</strong>: node and edge definitions with full
						TypeScript type inference
					</li>
					<li>
						<strong>Fast</strong>: 125 ns key lookups, 208 ns one-hop
						traversals, and 34 µs to commit 100 nodes (p50, Rust core, 10k nodes
						and 50k edges; see <a href="/docs/benchmarks">benchmarks</a>)
					</li>
					<li>
						<strong>ACID transactions</strong>: atomic commits backed by a
						write-ahead log
					</li>
				</ul>

				<h2 id="quick-example">Quick example</h2>
				<CodeBlock
					code={`import { kite, defineNode, defineEdge, string, vector, createVectorIndex } from '@kitedb/core';

const user = defineNode('user', {
  key: (id: string) => \`user:\${id}\`,
  props: {
    name: string('name'),
    embedding: vector('embedding', 1536),
  },
});

const follows = defineEdge('follows');

const db = await kite('./social.kitedb', {
  nodes: [user],
  edges: [follows],
});

// Create users
const [alice, bob] = db
  .insert(user)
  .valuesMany([
    { key: 'alice', name: 'Alice', embedding: [...] },
    { key: 'bob', name: 'Bob', embedding: [...] },
  ])
  .returning();

// Vector search (standalone index)
const index = createVectorIndex({ dimensions: 1536 });
index.set(alice.id, alice.embedding);
index.set(bob.id, bob.embedding);
index.buildIndex();

const similar = index.search(queryEmbedding, { k: 5 });`}
					language="typescript"
				/>

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/getting-started/installation">Installation</a>: add
						the package for your language
					</li>
					<li>
						<a href="/docs/getting-started/quick-start">Quick start</a>: build a
						small social graph
					</li>
					<li>
						<a href="/docs/guides/schema">Schema definition</a>: design your
						data model
					</li>
				</ul>
			</DocPage>
		);
	}

	// Default fallback for unknown pages
	return (
		<DocPage slug={slug}>
			<p>This page is coming soon.</p>
		</DocPage>
	);
}

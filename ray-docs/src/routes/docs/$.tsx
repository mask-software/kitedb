import { createFileRoute, useLocation } from "@tanstack/solid-router";
import { Show } from "solid-js";
import DocPage from "~/components/doc-page";
import CodeBlock from "~/components/code-block";
import { findDocBySlug } from "~/lib/docs";

export const Route = createFileRoute("/docs/$")({
  component: DocSplatPage,
});

function DocSplatPage() {
  const location = useLocation();
  const slug = () => {
    const path = location().pathname;
    const match = path.match(/^\/docs\/(.+)$/);
    return match ? match[1] : "";
  };
  const doc = () => findDocBySlug(slug());

  return (
    <Show when={doc()} fallback={<DocNotFound slug={slug()} />}>
      <DocPageContent slug={slug()} />
    </Show>
  );
}

function DocNotFound(props: { slug: string }) {
  return (
    <div class="max-w-4xl mx-auto px-6 py-12">
      <div class="text-center">
        <h1 class="text-4xl font-extrabold text-slate-900 dark:text-white mb-4">
          Page Not Found
        </h1>
        <p class="text-lg text-slate-600 dark:text-slate-400 mb-8">
          The documentation page{" "}
          <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">
            {props.slug}
          </code>{" "}
          doesn't exist yet.
        </p>
        <a
          href="/docs"
          class="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
        >
          Back to Documentation
        </a>
      </div>
    </div>
  );
}

function DocPageContent(props: { slug: string }) {
  const slug = props.slug;

  // Introduction page (empty slug)
  if (slug === "") {
    return (
      <DocPage slug="">
        <p>
          Welcome to KiteDB, a high-performance embedded graph database with
          built-in vector search, designed for Bun and TypeScript.
        </p>

        <h2 id="what-is-kitedb">What is KiteDB?</h2>
        <p>
          KiteDB is an embedded graph database that combines the power of graph
          relationships with semantic vector search. It's designed for modern
          TypeScript applications that need:
        </p>
        <ul>
          <li>
            <strong>Graph relationships</strong> – Model complex connections
            between entities
          </li>
          <li>
            <strong>Vector search</strong> – Find semantically similar content
            using embeddings
          </li>
          <li>
            <strong>Type safety</strong> – Full TypeScript support with inferred
            types
          </li>
          <li>
            <strong>High performance</strong> – Optimized for Bun with native
            bindings
          </li>
          <li>
            <strong>Zero setup</strong> – No external database to manage
          </li>
        </ul>

        <h2 id="key-features">Key Features</h2>
        <ul>
          <li>
            <strong>Graph-native</strong> – First-class nodes, edges, and
            traversals
          </li>
          <li>
            <strong>Vector search</strong> – HNSW-indexed similarity queries
          </li>
          <li>
            <strong>Embedded</strong> – Runs in your process, no server needed
          </li>
          <li>
            <strong>Type-safe</strong> – Schemas with full TypeScript inference
          </li>
          <li>
            <strong>Fast</strong> – 833k ops/sec writes, sub-ms traversals
          </li>
          <li>
            <strong>ACID</strong> – Full transaction support
          </li>
        </ul>

        <h2 id="quick-example">Quick Example</h2>
        <CodeBlock
          code={`import { ray, defineNode, defineEdge, prop } from 'kitedb';

const user = defineNode('user', {
  key: (id: string) => \`user:\${id}\`,
  props: {
    name: prop.string('name'),
    embedding: prop.vector('embedding', 1536),
  },
});

const follows = defineEdge('follows', {
  from: user,
  to: user,
});

const db = await ray('./social.kitedb', {
  nodes: [user],
  edges: [follows],
});

// Create users
await db.node(user).createMany([
  { id: 'alice', name: 'Alice', embedding: [...] },
  { id: 'bob', name: 'Bob', embedding: [...] },
]);

// Find similar users
const similar = await db.node(user)
  .vector('embedding')
  .similar(queryEmbedding, { limit: 5 })
  .all();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/getting-started/installation">Installation</a> – Get
            KiteDB set up
          </li>
          <li>
            <a href="/docs/getting-started/quick-start">Quick Start</a> – Build
            your first graph
          </li>
          <li>
            <a href="/docs/guides/schema">Schema Definition</a> – Design your
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

import { createFileRoute } from "@tanstack/solid-router";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { MultiLangCode } from "~/components/multi-lang-code";
import { loadDocSlug } from "~/lib/doc-route";

export const Route = createFileRoute("/docs/getting-started/$")({
	loader: loadDocSlug,
	component: GettingStartedSplatPage,
	notFoundComponent: () => <DocNotFound />,
});

function GettingStartedSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

function DocPageContent(props: { slug: string }) {
	const slug = props.slug;

	if (slug === "getting-started/quick-start") {
		return (
			<DocPage slug={slug}>
				<p>
					This guide builds a small social graph of users who follow each other.
					It covers defining a schema, writing nodes and edges, querying them,
					and closing the database.
				</p>

				<h2 id="create-schema">1. Define your schema</h2>
				<p>
					A schema lists the node and edge types and their properties. This one
					has a <code>user</code> node type and a <code>follows</code> edge
					type.
				</p>
				<MultiLangCode
					typescript={`import { kite } from '@kitedb/core';

// Define schema inline when opening the database
const db = await kite('./social.kitedb', {
  nodes: [
    {
      name: 'user',
      props: {
        name: { type: 'string' },
        email: { type: 'string' },
      },
    },
  ],
  edges: [
    {
      name: 'follows',
      props: {
        followedAt: { type: 'int' },  // Unix timestamp
      },
    },
  ],
});`}
					rust={`use kitedb::api::kite::{kite, EdgeDef, KiteOptions, NodeDef, PropDef};

// Define schema when opening the database
let mut db = kite(
    "./social.kitedb",
    KiteOptions::new()
        .node(
            NodeDef::new("user", "user:")
                .prop(PropDef::string("name"))
                .prop(PropDef::string("email")),
        )
        .edge(EdgeDef::new("follows").prop(PropDef::int("followedAt"))),
)?;`}
					python={`from kitedb import kite, define_node, define_edge, prop

# Define schema
user = define_node("user",
    key=lambda id: f"user:{id}",
    props={
        "name": prop.string("name"),
        "email": prop.string("email"),
    }
)

follows = define_edge("follows", {
    "followedAt": prop.int("followedAt"),
})

# Open database with schema
db = kite("./social.kitedb", nodes=[user], edges=[follows])`}
					filename={{ ts: "social.ts", rs: "main.rs", py: "social.py" }}
				/>

				<h2 id="add-data">2. Add some data</h2>
				<MultiLangCode
					typescript={`// Create users
const alice = db.insert('user')
  .values('alice', { name: 'Alice Chen', email: 'alice@example.com' })
  .returning();

const bob = db.insert('user')
  .values('bob', { name: 'Bob Smith', email: 'bob@example.com' })
  .returning();

// Create a follow relationship
db.link(alice.id, 'follows', bob.id, {
  followedAt: Math.floor(Date.now() / 1000),
});`}
					rust={`use kitedb::types::PropValue;
use std::collections::HashMap;

// Create users
let alice = db
    .insert("user")?
    .values("alice", HashMap::from([
        ("name".into(), PropValue::String("Alice Chen".into())),
        ("email".into(), PropValue::String("alice@example.com".into())),
    ]))?
    .returning()?;

let bob = db
    .insert("user")?
    .values("bob", HashMap::from([
        ("name".into(), PropValue::String("Bob Smith".into())),
        ("email".into(), PropValue::String("bob@example.com".into())),
    ]))?
    .returning()?;

// Create a follow relationship
let followed_at = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)?
    .as_secs() as i64;
db.link_with_props(alice.id(), "follows", bob.id(), HashMap::from([
    ("followedAt".into(), PropValue::I64(followed_at)),
]))?;`}
					python={`# Create users
alice = (db.insert(user)
    .values(key="alice", name="Alice Chen", email="alice@example.com")
    .returning())

bob = (db.insert(user)
    .values(key="bob", name="Bob Smith", email="bob@example.com")
    .returning())

# Create a follow relationship
import time
db.link(alice, follows, bob, followedAt=int(time.time()))`}
				/>

				<h2 id="query">3. Query the graph</h2>
				<MultiLangCode
					typescript={`// Find all users Alice follows
const following = db
  .from(alice.id)
  .out('follows')
  .nodes();

console.log('Alice follows:', following.length, 'users');

// Check if Alice follows Bob
const followsBob = db.hasEdge(alice.id, 'follows', bob.id);
console.log('Alice follows Bob:', followsBob);`}
					rust={`// Find all users Alice follows
let following = db
    .from(alice.id())
    .out(Some("follows"))?
    .to_vec();

println!("Alice follows: {} users", following.len());

// Check if Alice follows Bob
let follows_bob = db.has_edge(alice.id(), "follows", bob.id())?;
println!("Alice follows Bob: {}", follows_bob);`}
					python={`# Find all users Alice follows
following = (db
    .from_(alice)
    .out(follows)
    .nodes()
    .to_list())

print(f"Alice follows: {len(following)} users")

# Check if Alice follows Bob
follows_bob = db.has_edge(alice, follows, bob)
print(f"Alice follows Bob: {follows_bob}")`}
				/>

				<h2 id="cleanup">4. Close the database</h2>
				<MultiLangCode
					typescript={`// Always close when done
db.close();`}
					rust={`// Close when done
db.close()?;`}
					python={`# Close when done (or use context manager)
db.close()

# Better: use context manager
with kite("./social.kitedb", nodes=[user], edges=[follows]) as db:
    # ... operations ...
    pass  # Auto-closes on exit`}
				/>

				<h2 id="next-steps">Next steps</h2>
				<p>Each of these guides goes deeper on one step from this page:</p>
				<ul>
					<li>
						<a href="/docs/guides/schema">Schema definition</a>: property types
						and edge properties
					</li>
					<li>
						<a href="/docs/guides/queries">Queries & CRUD</a>: reading,
						updating, and deleting nodes
					</li>
					<li>
						<a href="/docs/guides/traversal">Graph traversal</a>: multi-hop and
						variable-depth queries
					</li>
					<li>
						<a href="/docs/guides/vectors">Vector search</a>: similarity search
						over embeddings
					</li>
				</ul>
			</DocPage>
		);
	}

	// Default fallback
	return (
		<DocPage slug={slug}>
			<p>This getting started guide is coming soon.</p>
		</DocPage>
	);
}

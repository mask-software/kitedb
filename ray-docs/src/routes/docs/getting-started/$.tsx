import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import { MultiLangCode } from '~/components/multi-lang-code'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/getting-started/$')({
  component: GettingStartedSplatPage,
})

function GettingStartedSplatPage() {
  const location = useLocation()
  const slug = () => {
    const path = location().pathname
    const match = path.match(/^\/docs\/(.+)$/)
    return match ? match[1] : ''
  }
  const doc = () => findDocBySlug(slug())

  return (
    <Show
      when={doc()}
      fallback={<DocNotFound slug={slug()} />}
    >
      <DocPageContent slug={slug()} />
    </Show>
  )
}

function DocNotFound(props: { slug: string }) {
  return (
    <div class="max-w-4xl mx-auto px-6 py-12">
      <div class="text-center">
        <h1 class="text-4xl font-extrabold text-slate-900 dark:text-white mb-4">
          Page Not Found
        </h1>
        <p class="text-lg text-slate-600 dark:text-slate-400 mb-8">
          The page <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
        </p>
        <a
          href="/docs"
          class="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
        >
          Back to Documentation
        </a>
      </div>
    </div>
  )
}

function DocPageContent(props: { slug: string }) {
  const slug = props.slug

  if (slug === 'getting-started/quick-start') {
    return (
      <DocPage slug={slug}>
        <p>
          Let's build a simple social graph database with users and their connections. 
          By the end of this guide, you'll understand the core concepts of RayDB.
        </p>

        <h2 id="create-schema">1. Define Your Schema</h2>
        <p>
          RayDB uses a schema to define nodes and edges. Let's create a simple 
          social network with users and follow relationships.
        </p>
        <MultiLangCode
          typescript={`import { ray } from '@ray-db/core';

// Define schema inline when opening the database
const db = ray('./social.raydb', {
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
          rust={`use raydb::ray;

// Define schema when opening the database
let db = ray("./social.raydb", RayOptions {
    nodes: vec![
        NodeSpec::new("user")
            .prop("name", PropType::String)
            .prop("email", PropType::String),
    ],
    edges: vec![
        EdgeSpec::new("follows")
            .prop("followedAt", PropType::Int),
    ],
    ..Default::default()
})?;`}
          python={`from raydb import ray, define_node, define_edge, prop

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
db = ray("./social.raydb", nodes=[user], edges=[follows])`}
          filename={{ ts: 'social.ts', rs: 'main.rs', py: 'social.py' }}
        />

        <h2 id="add-data">2. Add Some Data</h2>
        <MultiLangCode
          typescript={`// Create users
const alice = db.insert('user')
  .values('alice', { name: 'Alice Chen', email: 'alice@example.com' })
  .returning();

const bob = db.insert('user')
  .values('bob', { name: 'Bob Smith', email: 'bob@example.com' })
  .returning();

// Create a follow relationship
db.link(alice.id, 'follows', bob.id, { followedAt: Date.now() });`}
          rust={`// Create users
let alice = db.insert("user")
    .values("alice", json!({
        "name": "Alice Chen",
        "email": "alice@example.com"
    }))
    .returning()?;

let bob = db.insert("user")
    .values("bob", json!({
        "name": "Bob Smith",
        "email": "bob@example.com"
    }))
    .returning()?;

// Create a follow relationship
db.link(alice.id, "follows", bob.id, Some(json!({
    "followedAt": std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
})))?;`}
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

        <h2 id="query">3. Query the Graph</h2>
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
    .from(alice.id)
    .out(Some("follows"))
    .nodes()?;

println!("Alice follows: {} users", following.len());

// Check if Alice follows Bob
let follows_bob = db.has_edge(alice.id, "follows", bob.id)?;
println!("Alice follows Bob: {}", follows_bob);`}
          python={`# Find all users Alice follows
following = (db
    .from_(alice)
    .out(follows)
    .nodes()
    .to_list())

print(f"Alice follows: {len(following)} users")

# Check if Alice follows Bob
follows_bob = db.has_edge(alice.id, "follows", bob.id)
print(f"Alice follows Bob: {follows_bob}")`}
        />

        <h2 id="cleanup">4. Close the Database</h2>
        <MultiLangCode
          typescript={`// Always close when done
db.close();`}
          rust={`// Close when done (or use Drop)
db.close();`}
          python={`# Close when done (or use context manager)
db.close()

# Better: use context manager
with ray("./social.raydb", nodes=[user], edges=[follows]) as db:
    # ... operations ...
    pass  # Auto-closes on exit`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <p>
          Congratulations! You've built your first graph database with RayDB. 
          Continue learning with these guides:
        </p>
        <ul>
          <li><a href="/docs/guides/schema">Schema Definition</a> – Advanced schema patterns</li>
          <li><a href="/docs/guides/queries">Queries & CRUD</a> – All query operations</li>
          <li><a href="/docs/guides/vectors">Vector Search</a> – Semantic similarity</li>
        </ul>
      </DocPage>
    )
  }

  // Default fallback
  return (
    <DocPage slug={slug}>
      <p>This getting started guide is coming soon.</p>
    </DocPage>
  )
}

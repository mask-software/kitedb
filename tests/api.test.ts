/**
 * Tests for the high-level Drizzle-style API
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { defineEdge, defineNode, ray, optional, prop } from "../src/index.ts";

// ============================================================================
// Schema Definition
// ============================================================================

const user = defineNode("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string("name"),
    age: prop.int("age"),
    email: prop.string("email"),
    bio: optional(prop.string("bio")),
  },
});

const company = defineNode("company", {
  key: (id: string) => `company:${id}`,
  props: {
    name: prop.string("name"),
    founded: prop.int("founded"),
  },
});

const knows = defineEdge("knows", {
  since: prop.int("since"),
  weight: optional(prop.float("weight")),
});

const worksAt = defineEdge("worksAt", {
  role: prop.string("role"),
  startDate: prop.int("startDate"),
});

const follows = defineEdge("follows");

// ============================================================================
// Tests
// ============================================================================

describe("High-Level API", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-api-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  describe("Database Lifecycle", () => {
    test("open and close database", async () => {
      const db = await ray(testDir, {
        nodes: [user, company],
        edges: [knows, worksAt, follows],
      });

      const stats = await db.stats();
      expect(stats.snapshotGen).toBe(0n);

      await db.close();
    });
  });

  describe("Node Operations", () => {
    test("insert single node", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({
          key: "alice",
          name: "Alice Smith",
          age: 30n,
          email: "alice@example.com",
        })
        .returning();

      expect(alice.$id).toBeDefined();
      expect(alice.$key).toBe("user:alice");
      expect(alice.name).toBe("Alice Smith");
      expect(alice.age).toBe(30n);

      await db.close();
    });

    test("insert multiple nodes", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const results = await db
        .insert(user)
        .values([
          { key: "alice", name: "Alice", age: 30n, email: "alice@ex.com" },
          { key: "bob", name: "Bob", age: 25n, email: "bob@ex.com" },
        ])
        .returning();

      const [alice, bob] = results;
      expect(alice.name).toBe("Alice");
      expect(bob.name).toBe("Bob");

      await db.close();
    });

    test("get node by key", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      await db
        .insert(user)
        .values({
          key: "alice",
          name: "Alice",
          age: 30n,
          email: "alice@ex.com",
        })
        .returning();

      const found = await db.get(user, "alice");
      expect(found).not.toBeNull();
      expect(found?.name).toBe("Alice");

      const notFound = await db.get(user, "unknown");
      expect(notFound).toBeNull();

      await db.close();
    });

    test("update node by reference", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({
          key: "alice",
          name: "Alice",
          age: 30n,
          email: "alice@ex.com",
        })
        .returning();

      await db.update(alice).set({ age: 31n }).execute();

      const updated = await db.get(user, "alice");
      expect(updated?.age).toBe(31n);

      await db.close();
    });

    test("delete node by reference", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({
          key: "alice",
          name: "Alice",
          age: 30n,
          email: "alice@ex.com",
        })
        .returning();

      const exists1 = await db.exists(alice);
      expect(exists1).toBe(true);

      await db.delete(alice);

      const exists2 = await db.exists(alice);
      expect(exists2).toBe(false);

      await db.close();
    });
  });

  describe("Edge Operations", () => {
    test("link nodes", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const bob = await db
        .insert(user)
        .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
        .returning();

      await db.link(alice, knows, bob, { since: 2020n });

      const hasEdge = await db.hasEdge(alice, knows, bob);
      expect(hasEdge).toBe(true);

      const noEdge = await db.hasEdge(bob, knows, alice);
      expect(noEdge).toBe(false);

      await db.close();
    });

    test("unlink nodes", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const bob = await db
        .insert(user)
        .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
        .returning();

      await db.link(alice, knows, bob, { since: 2020n });
      expect(await db.hasEdge(alice, knows, bob)).toBe(true);

      await db.unlink(alice, knows, bob);
      expect(await db.hasEdge(alice, knows, bob)).toBe(false);

      await db.close();
    });
  });

  describe("Traversal", () => {
    test("traverse outgoing edges", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const bob = await db
        .insert(user)
        .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
        .returning();

      const charlie = await db
        .insert(user)
        .values({
          key: "charlie",
          name: "Charlie",
          age: 35n,
          email: "c@ex.com",
        })
        .returning();

      await db.link(alice, knows, bob, { since: 2020n });
      await db.link(alice, knows, charlie, { since: 2021n });

      const friends = await db.from(alice).out(knows).toArray();
      expect(friends).toHaveLength(2);

      await db.close();
    });

    test("traverse with take limit", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      for (let i = 0; i < 5; i++) {
        const friend = await db
          .insert(user)
          .values({
            key: `friend${i}`,
            name: `Friend ${i}`,
            age: BigInt(20 + i),
            email: `f${i}@ex.com`,
          })
          .returning();
        await db.link(alice, knows, friend, { since: BigInt(2020 + i) });
      }

      const limited = await db.from(alice).out(knows).take(3).toArray();
      expect(limited).toHaveLength(3);

      await db.close();
    });

    test("count traversal results", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      for (let i = 0; i < 3; i++) {
        const friend = await db
          .insert(user)
          .values({
            key: `friend${i}`,
            name: `Friend ${i}`,
            age: BigInt(20 + i),
            email: `f${i}@ex.com`,
          })
          .returning();
        await db.link(alice, knows, friend, { since: BigInt(2020 + i) });
      }

      const count = await db.from(alice).out(knows).count();
      expect(count).toBe(3);

      await db.close();
    });

    test("get first result", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const bob = await db
        .insert(user)
        .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
        .returning();

      await db.link(alice, knows, bob, { since: 2020n });

      const first = await db.from(alice).out(knows).first();
      expect(first).not.toBeNull();
      expect(first?.$id).toBe(bob.$id);

      await db.close();
    });

    test("iterate with for-await", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      const alice = await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const bob = await db
        .insert(user)
        .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
        .returning();

      await db.link(alice, knows, bob, { since: 2020n });

      const results = [];
      for await (const friend of db.from(alice).out(knows)) {
        results.push(friend);
      }
      expect(results).toHaveLength(1);

      await db.close();
    });
  });

  describe("Transactions", () => {
    test("explicit transaction", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      await db.transaction(async (tx) => {
        const alice = await tx
          .insert(user)
          .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
          .returning();

        const bob = await tx
          .insert(user)
          .values({ key: "bob", name: "Bob", age: 25n, email: "b@ex.com" })
          .returning();

        await tx.link(alice, knows, bob, { since: 2024n });
      });

      const alice = await db.get(user, "alice");
      expect(alice).not.toBeNull();

      const bob = await db.get(user, "bob");
      expect(bob).not.toBeNull();

      await db.close();
    });

    test("transaction rollback on error", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      try {
        await db.transaction(async (tx) => {
          await tx
            .insert(user)
            .values({
              key: "alice",
              name: "Alice",
              age: 30n,
              email: "a@ex.com",
            })
            .returning();

          throw new Error("Intentional error");
        });
      } catch {
        // Expected
      }

      const alice = await db.get(user, "alice");
      expect(alice).toBeNull();

      await db.close();
    });
  });

  describe("Maintenance", () => {
    test("stats", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      const stats = await db.stats();
      expect(stats.deltaNodesCreated).toBe(1n);

      await db.close();
    });

    test("optimize (compaction)", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      await db.optimize();

      const stats = await db.stats();
      expect(stats.snapshotGen).toBe(1n);
      expect(stats.snapshotNodes).toBe(1n);
      expect(stats.deltaNodesCreated).toBe(0n);

      await db.close();
    });

    test("check integrity", async () => {
      const db = await ray(testDir, {
        nodes: [user],
        edges: [knows],
      });

      await db
        .insert(user)
        .values({ key: "alice", name: "Alice", age: 30n, email: "a@ex.com" })
        .returning();

      await db.optimize();

      const result = await db.check();
      expect(result.valid).toBe(true);

      await db.close();
    });
  });
});

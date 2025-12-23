import { defineEdge, defineNode, optional, prop, ray } from "../src/index.ts";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const user = defineNode("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string("name"),
    email: prop.string("email"),
    age: optional(prop.int("age")),
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
});

const worksAt = defineEdge("worksAt", {
  role: prop.string("role"),
  startDate: prop.int("startDate"),
});

async function main() {
  const dir = await mkdtemp(join(tmpdir(), "ray-example-"));
  const db = await ray(dir, { nodes: [user, company], edges: [knows, worksAt] });

  try {
    const alice = await db
      .insert(user)
      .values({
        key: "alice",
        name: "Alice",
        email: "alice@example.com",
        age: 30n,
      })
      .returning();

    const bob = await db
      .insert(user)
      .values({
        key: "bob",
        name: "Bob",
        email: "bob@example.com",
        age: 25n,
      })
      .returning();

    const acme = await db
      .insert(company)
      .values({
        key: "acme",
        name: "Acme Co",
        founded: 1999n,
      })
      .returning();

    await db.link(alice, knows, bob, { since: 2020n });
    await db.link(alice, worksAt, acme, { role: "Engineer", startDate: 2022n });

    await db
      .update(user)
      .set({ email: "alice@new.com" })
      .where({ $key: "user:alice" })
      .execute();

    await db.update(alice).set({ age: 31n }).execute();

    const youngFriends = await db
      .from(alice)
      .out(knows)
      .whereNode((n) => (n.age ?? 0n) < 35n)
      .nodes()
      .toArray();

    console.log(
      "youngFriends",
      youngFriends.map((n) => n.$key),
    );

    await db.delete(user).where({ $key: "user:bob" }).execute();
  } finally {
    await db.close();
    await rm(dir, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});

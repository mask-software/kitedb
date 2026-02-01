import { edge, node, ray, optional, prop } from "@kitedb/core";

const user = node("user", {
  key: (id: string) => `user:${id}`,
  props: {
    name: prop.string("name"),
    email: prop.string("email"),
    age: optional(prop.int("age")),
  },
});

const follows = edge("follows", {
  since: prop.int("since"),
});

async function main(): Promise<void> {
  const db = await ray("./social.kitedb", {
    nodes: [user],
    edges: [follows],
  });

  const alice = db
    .insert(user)
    .values({
      key: "alice",
      name: "Alice",
      email: "alice@example.com",
      age: 30,
    })
    .returning();

  console.log("Inserted:", alice);

  await db.close();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});

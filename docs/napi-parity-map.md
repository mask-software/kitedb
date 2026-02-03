# NAPI Parity Map (Draft)

Goal: map the TypeScript API surface to a NAPI design that preserves
behavior while working within JS <-> Rust constraints.

## Surface Mapping (TS -> NAPI)

- Single-file only: use `Database.open(path, options)`
- Low-level database helpers (nodes/edges/props/cache) -> `Database` methods
- `VectorIndex` -> `VectorIndex` (nodeId-only until Kite/NodeRef exists)
- High-level `kite(path, options)` -> `kite(path, options)` returning `Kite`
- High-level builders/traversal/pathfinding -> NAPI `Kite` methods + builders

## Schema Input Format (NAPI)

NAPI cannot accept closures, so schema is JSON-compatible:

```ts
type SchemaInput = {
  nodes: NodeSchemaInput[];
  edges: EdgeSchemaInput[];
};

type NodeSchemaInput = {
  name: string;
  key?: KeySpec; // default: { kind: "prefix", prefix: `${name}:` }
  props: Record<string, PropSpec>;
};

type EdgeSchemaInput = {
  name: string;
  props?: Record<string, PropSpec>;
};

type PropSpec = {
  type: "string" | "int" | "float" | "bool" | "vector";
  optional?: boolean;
  default?: string | number | boolean | null;
};
```

### Key Generator Support

Key generation is expressed via a `KeySpec` object so we can rebuild a Rust
closure internally:

```ts
type KeySpec =
  | { kind: "prefix"; prefix?: string }
  | { kind: "template"; template: string }
  | { kind: "parts"; fields: string[]; separator?: string; prefix?: string };
```

Rules:
- `prefix`: key = `${prefix ?? name + ":"}${id}`
- `template`: replace `{field}` using a key args object
- `parts`: join `fields` from a key args object with `separator ?? ":"`

### CRUD Key Inputs

NAPI accepts either a precomputed key string or a key args object, depending
on the `KeySpec`:

```ts
kite.get(user, "user:alice");
kite.get(user, { id: "alice" }); // for template/parts
```

## Notes / Open Questions

- NodeRef availability in NAPI depends on the high-level Kite API landing.
- `template`/`parts` should be kept minimal to avoid ambiguous key generation.
- The schema input format should round-trip into Rust `NodeSchema`/`EdgeSchema`.

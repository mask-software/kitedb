/**
 * Schema Definition API
 *
 * Drizzle-style schema builders for defining graph nodes and edges
 * with full TypeScript type inference. Provides:
 *
 * - Property type builders (string, int, float, bool)
 * - Node type definitions with key generation
 * - Edge type definitions with optional properties
 * - Full type inference for insert, return, and edge property types
 *
 * @example
 * ```ts
 * const user = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     age: optional(prop.int('age')),
 *   },
 * });
 *
 * const knows = defineEdge('knows', {
 *   since: prop.int('since'),
 * });
 *
 * // Type is automatically inferred:
 * type InsertUser = InferNodeInsert<typeof user>;
 * // { key: string; name: string; age?: bigint; }
 *
 * type ReturnedUser = InferNode<typeof user>;
 * // { $id: bigint; $key: string; name: string; age?: bigint; }
 * ```
 */

import type { PropValueTag } from "../types.ts";

// ============================================================================
// Property Definition Types
// ============================================================================

/**
 * Property type identifiers
 *
 * - "string" → stores strings, maps to PropValueTag.STRING
 * - "int" → stores 64-bit signed integers (bigint), maps to PropValueTag.I64
 * - "float" → stores 64-bit floats, maps to PropValueTag.F64
 * - "bool" → stores booleans, maps to PropValueTag.BOOL
 */
export type PropType = "string" | "int" | "float" | "bool";

/**
 * Maps PropType strings to their corresponding TypeScript types:
 * - "string" → string
 * - "int" → bigint
 * - "float" → number
 * - "bool" → boolean
 */
export type PropTypeToTS<T extends PropType> = T extends "string"
  ? string
  : T extends "int"
    ? bigint
    : T extends "float"
      ? number
      : T extends "bool"
        ? boolean
        : never;

/**
 * A property definition with type and optionality information.
 * Holds metadata about a property (name, type, whether it's optional).
 */
export interface PropDef<
  T extends PropType = PropType,
  Optional extends boolean = false,
> {
  readonly _tag: "prop";
  readonly name: string;
  readonly type: T;
  readonly optional: Optional;
}

/**
 * An optional property definition.
 * Used in property schemas to mark properties that may be omitted.
 */
export interface OptionalPropDef<T extends PropType> extends PropDef<T, true> {
  readonly optional: true;
}

/**
 * A property builder that supports chaining `.optional()`.
 * Used by the `prop` object to build property definitions.
 */
export interface PropBuilder<T extends PropType> {
  readonly _tag: "prop";
  readonly name: string;
  readonly type: T;
  readonly optional: false;
  /** Convert this property to an optional property */
  makeOptional(): OptionalPropDef<T>;
}

// ============================================================================
// Property Builders
// ============================================================================

function createPropBuilder<T extends PropType>(
  name: string,
  type: T,
): PropBuilder<T> {
  return {
    _tag: "prop" as const,
    name,
    type,
    optional: false as const,
    makeOptional(): OptionalPropDef<T> {
      return {
        _tag: "prop",
        name,
        type,
        optional: true,
      } as OptionalPropDef<T>;
    },
  };
}

/**
 * Property type builders
 *
 * Use these to define typed properties on nodes and edges. All builders
 * support `.optional()` or the `optional()` helper for optional properties.
 *
 * @example
 * ```ts
 * const name = prop.string('name');
 * const age = prop.int('age');
 * const score = prop.float('score').optional();
 * const active = prop.bool('active');
 * ```
 */
export const prop = {
  /**
   * String property
   * Stored as UTF-8 strings (maps to PropValueTag.STRING)
   *
   * @param name - Property name
   * @returns Property builder that can be chained with `.optional()`
   */
  string: (name: string): PropBuilder<"string"> =>
    createPropBuilder(name, "string"),

  /**
   * Integer property as bigint
   * Stored as 64-bit signed integers (maps to PropValueTag.I64)
   *
   * @param name - Property name
   * @returns Property builder that can be chained with `.optional()`
   */
  int: (name: string): PropBuilder<"int"> => createPropBuilder(name, "int"),

  /**
   * Float property as number
   * Stored as 64-bit IEEE 754 floats (maps to PropValueTag.F64)
   *
   * @param name - Property name
   * @returns Property builder that can be chained with `.optional()`
   */
  float: (name: string): PropBuilder<"float"> =>
    createPropBuilder(name, "float"),

  /**
   * Boolean property
   * Stored as true/false (maps to PropValueTag.BOOL)
   *
   * @param name - Property name
   * @returns Property builder that can be chained with `.optional()`
   */
  bool: (name: string): PropBuilder<"bool"> => createPropBuilder(name, "bool"),
};

/**
 * Helper to make a property optional using function syntax
 * Alternative to `.makeOptional()` method chaining.
 *
 * @param p - Property builder to make optional
 * @returns Optional property definition
 *
 * @example
 * ```ts
 * const age = optional(prop.int('age'));
 * // equivalent to:
 * const age = prop.int('age').optional();
 * ```
 */
export function optional<T extends PropType>(
  p: PropBuilder<T>,
): OptionalPropDef<T> {
  return p.makeOptional();
}

// ============================================================================
// Node Definition Types
// ============================================================================

/**
 * Property schema for a node
 * A record of property names to property definitions
 */
export type PropsSchema = Record<string, PropDef<PropType, boolean>>;

/**
 * Extracts TypeScript types from a props schema, separating required and optional.
 * Required properties become non-optional fields, optional ones become optional fields.
 *
 * @example
 * ```ts
 * const schema = {
 *   name: prop.string('name'),
 *   age: optional(prop.int('age')),
 * };
 *
 * type Props = InferPropsType<typeof schema>;
 * // { name: string; age?: bigint; }
 * ```
 */
export type InferPropsType<P extends PropsSchema> = {
  [K in keyof P as P[K]["optional"] extends true ? never : K]: PropTypeToTS<
    P[K]["type"]
  >;
} & {
  [K in keyof P as P[K]["optional"] extends true ? K : never]?: PropTypeToTS<
    P[K]["type"]
  >;
};

/**
 * Node definition configuration
 * Specifies how to generate keys and which properties are available
 */
export interface NodeConfig<
  KeyArg extends string | number = string,
  P extends PropsSchema = PropsSchema,
> {
  /**
   * Key generator function
   * Transforms application IDs into unique node keys
   *
   * @param id - Application identifier (string or number)
   * @returns The full node key
   */
  key: (id: KeyArg) => string;
  /**
   * Property definitions for this node type
   */
  props: P;
}

/**
 * A defined node type with metadata
 * Created by `defineNode()` and used throughout the API
 */
export interface NodeDef<
  Name extends string = string,
  KeyArg extends string | number = string,
  P extends PropsSchema = PropsSchema,
> {
  readonly _tag: "node";
  readonly name: Name;
  readonly keyFn: (id: KeyArg) => string;
  readonly props: P;
  /** @internal Resolved prop key IDs (set during db initialization) */
  _propKeyIds?: Map<string, number>;
}

/**
 * Infers the value type for inserting a node
 *
 * Includes the `key` field (transformed by key function) plus all properties
 *
 * @example
 * ```ts
 * const user = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     age: optional(prop.int('age')),
 *   },
 * });
 *
 * type Insert = InferNodeInsert<typeof user>;
 * // { key: string; name: string; age?: bigint; }
 *
 * const user = await db.insert(user).values({
 *   key: 'alice',
 *   name: 'Alice',
 * }).returning();
 * ```
 */
export type InferNodeInsert<N extends NodeDef> =
  N extends NodeDef<string, infer KeyArg, infer P>
    ? { key: KeyArg } & InferPropsType<P>
    : never;

/**
 * Infers the return type for a node query
 *
 * Includes system fields (`$id`, `$key`) plus all properties.
 * All returned nodes have these system fields.
 *
 * @example
 * ```ts
 * const user = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     age: optional(prop.int('age')),
 *   },
 * });
 *
 * type Returned = InferNode<typeof user>;
 * // { $id: bigint; $key: string; name: string; age?: bigint; }
 *
 * const result = await db.get(user, 'alice');
 * // result: { $id: 1n, $key: 'user:alice', name: 'Alice', age: undefined }
 * ```
 */
export type InferNode<N extends NodeDef> =
  N extends NodeDef<string, infer _KeyArg, infer P>
    ? { $id: bigint; $key: string } & InferPropsType<P>
    : never;

// ============================================================================
// Edge Definition Types
// ============================================================================

/**
 * Edge definition properties schema
 * A record of property names to property definitions for edges
 */
export type EdgePropsSchema = Record<string, PropDef<PropType, boolean>>;

/**
 * Empty edge properties
 * Used when defining edges with no properties
 */
export type EmptyEdgeProps = Record<string, never>;

/**
 * A defined edge type with metadata
 * Created by `defineEdge()` and used throughout the API
 */
export interface EdgeDef<
  Name extends string = string,
  P extends EdgePropsSchema = EdgePropsSchema,
> {
  readonly _tag: "edge";
  readonly name: Name;
  readonly props: P;
  /** @internal Resolved edge type ID (set during db initialization) */
  _etypeId?: number;
  /** @internal Resolved prop key IDs (set during db initialization) */
  _propKeyIds?: Map<string, number>;
}

/**
 * Infers the props type for creating or updating an edge
 *
 * @example
 * ```ts
 * const knows = defineEdge('knows', {
 *   since: prop.int('since'),
 *   weight: optional(prop.float('weight')),
 * });
 *
 * type Props = InferEdgeProps<typeof knows>;
 * // { since: bigint; weight?: number; }
 *
 * await db.link(alice, knows, bob, {
 *   since: 2020n,
 *   weight: 0.95,
 * });
 * ```
 */
export type InferEdgeProps<E extends EdgeDef> =
  E extends EdgeDef<string, infer P> ? InferPropsType<P> : never;

/**
 * Infers the full edge type including source, destination, and properties
 *
 * @example
 * ```ts
 * const knows = defineEdge('knows', {
 *   since: prop.int('since'),
 * });
 *
 * type Edge = InferEdge<typeof knows>;
 * // { $src: bigint; $dst: bigint; since: bigint; }
 *
 * const edges = await db.from(alice).out(knows).edges().toArray();
 * // Each edge: { $src: 1n, $dst: 2n, since: 2020n }
 * ```
 */
export type InferEdge<E extends EdgeDef> =
  E extends EdgeDef<string, infer P>
    ? { $src: bigint; $dst: bigint } & InferPropsType<P>
    : never;

// ============================================================================
// Node and Edge Builders
// ============================================================================

/**
 * Define a node type with properties
 *
 * Creates a node definition that can be used for all node operations
 * (insert, update, delete, query). Provides full type inference for
 * insert values and return types.
 *
 * @typeParam Name - The node type name (must be unique per schema)
 * @typeParam KeyArg - The type of the key argument (string or number)
 * @typeParam P - The properties schema
 *
 * @param name - The node type name (must be unique)
 * @param config - Configuration with key function and properties
 * @returns A NodeDef that can be used with the database API
 *
 * @example
 * ```ts
 * const user = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     email: prop.string('email'),
 *     age: optional(prop.int('age')),
 *   },
 * });
 *
 * // Type inference:
 * // InferNodeInsert<typeof user> = { key: string; name: string; email: string; age?: bigint; }
 * // InferNode<typeof user> = { $id: bigint; $key: string; name: string; email: string; age?: bigint; }
 * ```
 */
export function defineNode<
  Name extends string,
  KeyArg extends string | number,
  P extends PropsSchema,
>(name: Name, config: NodeConfig<KeyArg, P>): NodeDef<Name, KeyArg, P> {
  return {
    _tag: "node",
    name,
    keyFn: config.key,
    props: config.props,
  };
}

/**
 * Define an edge type with properties
 *
 * Creates an edge definition that can be used for all edge operations
 * (link, unlink, query). Edges are directional and can have properties.
 *
 * @typeParam Name - The edge type name (must be unique per schema)
 * @typeParam P - The properties schema
 *
 * @param name - The edge type name (must be unique)
 * @param props - Property definitions (optional)
 * @returns An EdgeDef that can be used with the database API
 *
 * @example
 * ```ts
 * // Edge with properties
 * const knows = defineEdge('knows', {
 *   since: prop.int('since'),
 *   weight: optional(prop.float('weight')),
 * });
 *
 * // Edge without properties
 * const follows = defineEdge('follows');
 *
 * // Type inference:
 * // InferEdgeProps<typeof knows> = { since: bigint; weight?: number; }
 * // InferEdge<typeof knows> = { $src: bigint; $dst: bigint; since: bigint; weight?: number; }
 * ```
 */
export function defineEdge<Name extends string, P extends EdgePropsSchema>(
  name: Name,
  props: P,
): EdgeDef<Name, P>;

/**
 * Define an edge type with no properties
 */
export function defineEdge<Name extends string>(
  name: Name,
): EdgeDef<Name, EmptyEdgeProps>;

export function defineEdge<Name extends string, P extends EdgePropsSchema>(
  name: Name,
  props?: P,
): EdgeDef<Name, P | EmptyEdgeProps> {
  return {
    _tag: "edge",
    name,
    props: props ?? ({} as EmptyEdgeProps),
  };
}

// ============================================================================
// Schema Type
// ============================================================================

/**
 * Complete schema configuration for a ray database
 * Defines all nodes and edges that can exist in the database
 */
export interface RaySchema {
  /** All node type definitions */
  nodes: NodeDef[];
  /** All edge type definitions */
  edges: EdgeDef[];
}

// ============================================================================
// Utility: Convert prop type to PropValueTag
// ============================================================================

/**
 * Converts a PropType string to its internal PropValueTag representation
 * @internal
 */
export function propTypeToTag(type: PropType): PropValueTag {
  switch (type) {
    case "string":
      return 4; // PropValueTag.STRING
    case "int":
      return 2; // PropValueTag.I64
    case "float":
      return 3; // PropValueTag.F64
    case "bool":
      return 1; // PropValueTag.BOOL
  }
}

// ============================================================================
// Re-export for convenience
// ============================================================================

/** Property value tag type from the core database */
export type { PropValueTag };

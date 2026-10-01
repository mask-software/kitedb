/**
 * Schema Definition API for KiteDB
 *
 * Provides type-safe schema builders for defining graph nodes and edges.
 *
 * @example
 * ```typescript
 * import { node, edge, string, int, optional } from '@kitedb/core'
 *
 * const User = node('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: string('name'),
 *     email: string('email'),
 *     age: optional(int('age')),
 *   },
 * })
 *
 * const knows = edge('knows', {
 *   since: int('since'),
 * })
 * ```
 */

// =============================================================================
// Property Types
// =============================================================================

/** Property type identifiers */
export type PropType = 'string' | 'int' | 'float' | 'bool' | 'vector' | 'any'

/** Property specification */
export interface PropSpec<T extends PropType = PropType> {
  /** Property type */
  type: T
  /** Whether this property is optional */
  optional?: boolean
  /** Default value for this property */
  default?: unknown
}

// =============================================================================
// Property Builders
// =============================================================================

/**
 * Property type builders.
 *
 * Use these to define typed properties on nodes and edges.
 *
 * @example
 * ```typescript
 * const name = string('name')        // required string
 * const age = optional(int('age'))   // optional int
 * const score = float('score')       // required float
 * const active = bool('active')      // required bool
 * const embedding = vector('embedding', 1536)  // vector with dimensions
 * ```
 */
export const prop = {
  /**
   * String property.
   * Stored as UTF-8 strings.
   */
  string: (_name: string): PropSpec<'string'> => ({ type: 'string' }),

  /**
   * Integer property.
   * Stored as 64-bit signed integers.
   */
  int: (_name: string): PropSpec<'int'> => ({ type: 'int' }),

  /**
   * Float property.
   * Stored as 64-bit IEEE 754 floats.
   */
  float: (_name: string): PropSpec<'float'> => ({ type: 'float' }),

  /**
   * Boolean property.
   */
  bool: (_name: string): PropSpec<'bool'> => ({ type: 'bool' }),

  /**
   * Vector property for embeddings.
   * Stored as Float32 arrays.
   *
   * @param _name - Property name
   * @param _dimensions - Vector dimensions (for documentation/validation)
   */
  vector: (_name: string, _dimensions?: number): PropSpec<'vector'> => ({ type: 'vector' }),

  /**
   * Any property (schema-less).
   * Accepts any value type.
   */
  any: (_name: string): PropSpec<'any'> => ({ type: 'any' }),
}

// Top-level property builders (sugar over prop.*)
export const string = prop.string
export const int = prop.int
export const float = prop.float
export const bool = prop.bool
export const vector = prop.vector
export const any = prop.any

/**
 * Mark a property as optional.
 *
 * @example
 * ```typescript
 * const age = optional(int('age'))
 * ```
 */
export function optional<T extends PropSpec>(spec: T): T & { optional: true } {
  return { ...spec, optional: true }
}

/**
 * Set a default value for a property.
 *
 * @example
 * ```typescript
 * const status = withDefault(string('status'), 'active')
 * ```
 */
export function withDefault<T extends PropSpec>(spec: T, value: unknown): T {
  return { ...spec, default: value }
}

// =============================================================================
// Key Specification
// =============================================================================

/** Key generation strategy */
export interface KeySpec {
  /** Key generation kind */
  kind: 'prefix' | 'template' | 'parts'
  /** Key prefix (for all kinds) */
  prefix?: string
  /** Template string with {field} placeholders (for 'template' kind) */
  template?: string
  /** Field names to concatenate (for 'parts' kind) */
  fields?: string[]
  /** Separator between parts (for 'parts' kind, default ':') */
  separator?: string
}

// =============================================================================
// Node Definition
// =============================================================================

/** Node type specification */
export interface NodeSpec<
  P extends Record<string, PropSpec> | undefined = Record<string, PropSpec> | undefined,
> {
  /** Node type name (must be unique per database) */
  name: string
  /** Key generation specification */
  key?: KeySpec
  /** Property definitions */
  props?: P
}

/**
 * Node objects carry their identity as `id`, `key` and `type`, so props must not
 * use these names: they would shadow the identity of every returned node.
 */
const RESERVED_NODE_PROPS: ReadonlyArray<string> = ['id', 'key', 'type']

/** Configuration for node() */
export interface NodeConfig<
  K extends string = string,
  P extends Record<string, PropSpec> | undefined = Record<string, PropSpec> | undefined,
> {
  /**
   * Key generator function or key specification.
   *
   * If a function is provided, it will be analyzed to extract the key prefix.
   *
   * @example
   * ```typescript
   * // Function form - prefix is extracted automatically
   * key: (id: string) => `user:${id}`
   *
   * // Object form - explicit specification
   * key: { kind: 'prefix', prefix: 'user:' }
   * key: { kind: 'template', template: 'user:{org}:{id}' }
   * key: { kind: 'parts', fields: ['org', 'id'], separator: ':' }
   * ```
   */
  key?: ((arg: K) => string) | KeySpec
  /** Property definitions */
  props?: P
}

/**
 * Define a node type with properties.
 *
 * Creates a node definition that can be used for all node operations
 * (insert, update, delete, query).
 *
 * @param name - The node type name (must be unique)
 * @param config - Node configuration with key function and properties
 * @returns A NodeSpec that can be passed to kite()
 * @throws If a prop is named `id`, `key` or `type` (reserved for node identity)
 *
 * @example
 * ```typescript
 * const User = node('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: string('name'),
 *     email: string('email'),
 *     age: optional(int('age')),
 *   },
 * })
 *
 * // With template key
 * const OrgUser = node('org_user', {
 *   key: { kind: 'template', template: 'org:{org}:user:{id}' },
 *   props: {
 *     name: string('name'),
 *   },
 * })
 * ```
 */
export function node<
  K extends string = string,
  P extends Record<string, PropSpec> | undefined = Record<string, PropSpec> | undefined,
>(name: string, config?: NodeConfig<K, P>): NodeSpec<P> {
  if (!config) {
    return { name }
  }

  for (const propName of Object.keys(config.props ?? {})) {
    if (RESERVED_NODE_PROPS.includes(propName)) {
      throw new Error(
        `node('${name}'): prop name '${propName}' is reserved for node identity (id, key, type); rename the prop`,
      )
    }
  }

  let keySpec: KeySpec | undefined

  if (typeof config.key === 'function') {
    // Extract prefix from key function by calling it with a test value
    const testKey = config.key('__test__' as K)
    const testIdx = testKey.indexOf('__test__')
    if (testIdx !== -1) {
      const prefix = testKey.slice(0, testIdx)
      keySpec = { kind: 'prefix', prefix }
    } else {
      // Couldn't extract prefix, use default
      keySpec = { kind: 'prefix', prefix: `${name}:` }
    }
  } else if (config.key) {
    keySpec = config.key
  }

  return {
    name,
    key: keySpec,
    props: config.props,
  }
}

// =============================================================================
// Edge Definition
// =============================================================================

/** Edge type specification */
export interface EdgeSpec<
  P extends Record<string, PropSpec> | undefined = Record<string, PropSpec> | undefined,
> {
  /** Edge type name (must be unique per database) */
  name: string
  /** Property definitions */
  props?: P
}

/**
 * Define an edge type with optional properties.
 *
 * Creates an edge definition that can be used for all edge operations
 * (link, unlink, query). Edges are directional and can have properties.
 *
 * @param name - The edge type name (must be unique)
 * @param props - Optional property definitions
 * @returns An EdgeSpec that can be passed to kite()
 *
 * @example
 * ```typescript
 * // Edge with properties
 * const knows = edge('knows', {
 *   since: int('since'),
 *   weight: optional(float('weight')),
 * })
 *
 * // Edge without properties
 * const follows = edge('follows')
 * ```
 */
export function edge<
  P extends Record<string, PropSpec> | undefined = Record<string, PropSpec> | undefined,
>(name: string, props?: P): EdgeSpec<P> {
  return { name, props }
}

// =============================================================================
// Aliases for backwards compatibility
// =============================================================================

/** @deprecated Use `node()` instead */
export const defineNode = node

/** @deprecated Use `edge()` instead */
export const defineEdge = edge

// =============================================================================
// Type Inference Helpers
// =============================================================================

type PropValue<S extends PropSpec> = S['type'] extends 'string'
  ? string
  : S['type'] extends 'int'
    ? number
    : S['type'] extends 'float'
      ? number
      : S['type'] extends 'bool'
        ? boolean
        : S['type'] extends 'vector'
          ? Array<number>
          : unknown

type OptionalKeys<P extends Record<string, PropSpec>> = {
  [K in keyof P]: P[K] extends { optional: true } ? K : never
}[keyof P]

type RequiredKeys<P extends Record<string, PropSpec>> = Exclude<keyof P, OptionalKeys<P>>

type PropsFromSpec<P extends Record<string, PropSpec> | undefined> = P extends Record<string, PropSpec>
  ? {
      [K in RequiredKeys<P>]: PropValue<P[K]>
    } & {
      [K in OptionalKeys<P>]?: PropValue<P[K]>
    }
  : Record<string, never>

export type NodeRef<N extends NodeSpec = NodeSpec> = {
  id: number
  key: string
  type: N['name']
}

export type InferNodeInsert<N extends NodeSpec> = {
  key: string
} & PropsFromSpec<N['props']>

export type InferNodeUpsert<N extends NodeSpec> = {
  key: string
} & Partial<PropsFromSpec<N['props']>>

export type InferNode<N extends NodeSpec> = NodeRef<N> & PropsFromSpec<N['props']>

export type InferEdgeProps<E extends EdgeSpec> = PropsFromSpec<E['props']>

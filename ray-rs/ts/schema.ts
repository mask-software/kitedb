/**
 * Schema Definition API for RayDB
 *
 * Provides type-safe schema builders for defining graph nodes and edges.
 *
 * @example
 * ```typescript
 * import { node, edge, prop, optional } from '@ray-db/core'
 *
 * const User = node('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     email: prop.string('email'),
 *     age: optional(prop.int('age')),
 *   },
 * })
 *
 * const knows = edge('knows', {
 *   since: prop.int('since'),
 * })
 * ```
 */

// =============================================================================
// Property Types
// =============================================================================

/** Property type identifiers */
export type PropType = 'string' | 'int' | 'float' | 'bool' | 'vector' | 'any'

/** Property specification */
export interface PropSpec {
  /** Property type */
  type: PropType
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
 * const name = prop.string('name')        // required string
 * const age = optional(prop.int('age'))   // optional int
 * const score = prop.float('score')       // required float
 * const active = prop.bool('active')      // required bool
 * const embedding = prop.vector('embedding', 1536)  // vector with dimensions
 * ```
 */
export const prop = {
  /**
   * String property.
   * Stored as UTF-8 strings.
   */
  string: (_name: string): PropSpec => ({ type: 'string' }),

  /**
   * Integer property.
   * Stored as 64-bit signed integers.
   */
  int: (_name: string): PropSpec => ({ type: 'int' }),

  /**
   * Float property.
   * Stored as 64-bit IEEE 754 floats.
   */
  float: (_name: string): PropSpec => ({ type: 'float' }),

  /**
   * Boolean property.
   */
  bool: (_name: string): PropSpec => ({ type: 'bool' }),

  /**
   * Vector property for embeddings.
   * Stored as Float32 arrays.
   *
   * @param _name - Property name
   * @param _dimensions - Vector dimensions (for documentation/validation)
   */
  vector: (_name: string, _dimensions?: number): PropSpec => ({ type: 'vector' }),

  /**
   * Any property (schema-less).
   * Accepts any value type.
   */
  any: (_name: string): PropSpec => ({ type: 'any' }),
}

/**
 * Mark a property as optional.
 *
 * @example
 * ```typescript
 * const age = optional(prop.int('age'))
 * ```
 */
export function optional<T extends PropSpec>(spec: T): T {
  return { ...spec, optional: true }
}

/**
 * Set a default value for a property.
 *
 * @example
 * ```typescript
 * const status = withDefault(prop.string('status'), 'active')
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
export interface NodeSpec {
  /** Node type name (must be unique per database) */
  name: string
  /** Key generation specification */
  key?: KeySpec
  /** Property definitions */
  props?: Record<string, PropSpec>
}

/** Configuration for node() */
export interface NodeConfig<K extends string = string> {
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
  props?: Record<string, PropSpec>
}

/**
 * Define a node type with properties.
 *
 * Creates a node definition that can be used for all node operations
 * (insert, update, delete, query).
 *
 * @param name - The node type name (must be unique)
 * @param config - Node configuration with key function and properties
 * @returns A NodeSpec that can be passed to ray()
 *
 * @example
 * ```typescript
 * const User = node('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: {
 *     name: prop.string('name'),
 *     email: prop.string('email'),
 *     age: optional(prop.int('age')),
 *   },
 * })
 *
 * // With template key
 * const OrgUser = node('org_user', {
 *   key: { kind: 'template', template: 'org:{org}:user:{id}' },
 *   props: {
 *     name: prop.string('name'),
 *   },
 * })
 * ```
 */
export function node<K extends string = string>(name: string, config?: NodeConfig<K>): NodeSpec {
  if (!config) {
    return { name }
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
export interface EdgeSpec {
  /** Edge type name (must be unique per database) */
  name: string
  /** Property definitions */
  props?: Record<string, PropSpec>
}

/**
 * Define an edge type with optional properties.
 *
 * Creates an edge definition that can be used for all edge operations
 * (link, unlink, query). Edges are directional and can have properties.
 *
 * @param name - The edge type name (must be unique)
 * @param props - Optional property definitions
 * @returns An EdgeSpec that can be passed to ray()
 *
 * @example
 * ```typescript
 * // Edge with properties
 * const knows = edge('knows', {
 *   since: prop.int('since'),
 *   weight: optional(prop.float('weight')),
 * })
 *
 * // Edge without properties
 * const follows = edge('follows')
 * ```
 */
export function edge(name: string, props?: Record<string, PropSpec>): EdgeSpec {
  return { name, props }
}

// =============================================================================
// Aliases for backwards compatibility
// =============================================================================

/** @deprecated Use `node()` instead */
export const defineNode = node

/** @deprecated Use `edge()` instead */
export const defineEdge = edge

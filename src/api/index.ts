/**
 * Ray High-Level API
 *
 * Drizzle-style, type-safe API for the Ray graph database.
 * Provides schema definition, query building, and graph traversal with
 * full TypeScript type inference.
 *
 * @module
 *
 * @example
 * ```ts
 * import { ray, defineNode, defineEdge, prop } from './src/api';
 *
 * const user = defineNode('user', {
 *   key: (id: string) => `user:${id}`,
 *   props: { name: prop.string('name') },
 * });
 *
 * const knows = defineEdge('knows');
 *
 * const db = await ray('./my-db', {
 *   nodes: [user],
 *   edges: [knows],
 * });
 * ```
 */

/**
 * Query builders for CRUD operations
 * Fluent API for insert, update, delete with where conditions
 * @see {@link InsertBuilder}
 * @see {@link UpdateBuilder}
 * @see {@link DeleteBuilder}
 */
export type {
  DeleteBuilder,
  DeleteExecutor,
  InsertBuilder,
  InsertExecutor,
  LinkExecutor,
  NodeRef,
  UpdateBuilder,
  UpdateByRefBuilder,
  UpdateByRefExecutor,
  UpdateEdgeBuilder,
  UpdateEdgeExecutor,
  UpdateExecutor,
  WhereCondition,
} from "./builders.ts";
/**
 * Main database context
 * @see {@link ray}
 * @see {@link Ray}
 */
export { Ray, type RayOptions, ray, type TransactionContext } from "./ray.ts";
/**
 * Schema definition builders
 * Define node and edge types with properties and type inference
 * @see {@link defineNode}
 * @see {@link defineEdge}
 * @see {@link prop}
 */
export {
  defineEdge,
  defineNode,
  type EdgeDef,
  type EdgePropsSchema,
  type InferEdge,
  type InferEdgeProps,
  type InferNode,
  type InferNodeInsert,
  type NodeDef,
  type OptionalPropDef,
  optional,
  type PropBuilder,
  type PropDef,
  type PropsSchema,
  prop,
  type RaySchema,
} from "./schema.ts";

/**
 * Graph traversal with filtering and aggregation
 * Chain traversal steps (out, in, both) with lazy async iteration
 * @see {@link TraversalBuilder}
 */
export type {
  AsyncTraversalResult,
  EdgeResult,
  TraversalBuilder,
  TraversalDirection,
  TraverseOptions,
} from "./traversal.ts";

/**
 * Path finding algorithms (Dijkstra, A*)
 * Find shortest paths between nodes with weighted edges
 * @see {@link PathFindingBuilder}
 */
export type {
  Heuristic,
  PathExecutor,
  PathFindingBuilder,
  PathResult,
  WeightSpec,
} from "./pathfinding.ts";

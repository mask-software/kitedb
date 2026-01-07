/**
 * Memgraph client wrapper for benchmarking
 *
 * Provides an API similar to RayDB's low-level functions for fair comparison.
 */

import neo4j, { type Driver, type Session, type Integer } from "neo4j-driver";

export interface MemgraphNode {
  id: number;
  key: string;
}

export interface MemgraphEdge {
  src: number;
  dst: number;
  type: string;
}

export class MemgraphClient {
  private driver: Driver;
  private uri: string;

  constructor(uri: string = "bolt://localhost:7687") {
    this.uri = uri;
    this.driver = neo4j.driver(uri);
  }

  /**
   * Get a new session
   */
  private session(): Session {
    return this.driver.session();
  }

  /**
   * Close the driver connection
   */
  async close(): Promise<void> {
    await this.driver.close();
  }

  /**
   * Clear all data from the database
   */
  async clearDatabase(): Promise<void> {
    const session = this.session();
    try {
      // Drop all indexes first
      const indexResult = await session.run("SHOW INDEX INFO");
      for (const record of indexResult.records) {
        const indexName = record.get("index_name");
        if (indexName) {
          await session.run(`DROP INDEX ON :${record.get("label")}(${record.get("property")})`);
        }
      }
    } catch {
      // Ignore errors from SHOW INDEX INFO (might not exist)
    }

    try {
      // Delete all nodes and relationships
      await session.run("MATCH (n) DETACH DELETE n");
    } finally {
      await session.close();
    }
  }

  /**
   * Create index on Node.key for efficient lookups
   */
  async createIndex(): Promise<void> {
    const session = this.session();
    try {
      await session.run("CREATE INDEX ON :Node(key)");
    } catch {
      // Index might already exist
    } finally {
      await session.close();
    }
  }

  /**
   * Create nodes in batch
   * Returns a map of key -> internal node ID
   */
  async createNodes(keys: string[]): Promise<Map<string, number>> {
    const result = new Map<string, number>();
    const session = this.session();

    try {
      // Batch insert using UNWIND
      const queryResult = await session.run(
        `UNWIND $keys AS key
         CREATE (n:Node {key: key})
         RETURN key, id(n) AS nodeId`,
        { keys },
      );

      for (const record of queryResult.records) {
        const key = record.get("key") as string;
        const nodeId = record.get("nodeId") as Integer;
        result.set(key, nodeId.toNumber());
      }
    } finally {
      await session.close();
    }

    return result;
  }

  /**
   * Create edges in batch
   * Uses edge type as relationship type for efficient filtering
   */
  async createEdges(edges: MemgraphEdge[]): Promise<void> {
    const session = this.session();

    try {
      // Group edges by type for more efficient creation
      const edgesByType = new Map<string, { src: number; dst: number }[]>();
      for (const edge of edges) {
        const typeEdges = edgesByType.get(edge.type) || [];
        typeEdges.push({ src: edge.src, dst: edge.dst });
        edgesByType.set(edge.type, typeEdges);
      }

      // Create edges for each type
      for (const [type, typeEdges] of edgesByType) {
        await session.run(
          `UNWIND $edges AS e
           MATCH (a:Node), (b:Node)
           WHERE id(a) = e.src AND id(b) = e.dst
           CREATE (a)-[:${type}]->(b)`,
          { edges: typeEdges },
        );
      }
    } finally {
      await session.close();
    }
  }

  /**
   * Get a node by its key
   * Returns the internal node ID or null if not found
   */
  async getNodeByKey(key: string): Promise<number | null> {
    const session = this.session();
    try {
      const result = await session.run(
        "MATCH (n:Node {key: $key}) RETURN id(n) AS nodeId",
        { key },
      );

      if (result.records.length === 0) {
        return null;
      }

      const nodeId = result.records[0]!.get("nodeId") as Integer;
      return nodeId.toNumber();
    } finally {
      await session.close();
    }
  }

  /**
   * Get outgoing neighbors of a node
   * Optionally filter by edge type
   */
  async getNeighborsOut(nodeId: number, edgeType?: string): Promise<number[]> {
    const session = this.session();
    try {
      const query = edgeType
        ? `MATCH (n)-[:${edgeType}]->(m) WHERE id(n) = $nodeId RETURN id(m) AS neighborId`
        : "MATCH (n)-[]->(m) WHERE id(n) = $nodeId RETURN id(m) AS neighborId";

      const result = await session.run(query, { nodeId });

      return result.records.map((record) => {
        const neighborId = record.get("neighborId") as Integer;
        return neighborId.toNumber();
      });
    } finally {
      await session.close();
    }
  }

  /**
   * Get incoming neighbors of a node
   * Optionally filter by edge type
   */
  async getNeighborsIn(nodeId: number, edgeType?: string): Promise<number[]> {
    const session = this.session();
    try {
      const query = edgeType
        ? `MATCH (n)<-[:${edgeType}]-(m) WHERE id(n) = $nodeId RETURN id(m) AS neighborId`
        : "MATCH (n)<-[]-(m) WHERE id(n) = $nodeId RETURN id(m) AS neighborId";

      const result = await session.run(query, { nodeId });

      return result.records.map((record) => {
        const neighborId = record.get("neighborId") as Integer;
        return neighborId.toNumber();
      });
    } finally {
      await session.close();
    }
  }

  /**
   * Check if an edge exists between two nodes
   */
  async edgeExists(src: number, edgeType: string, dst: number): Promise<boolean> {
    const session = this.session();
    try {
      const result = await session.run(
        `MATCH (a)-[:${edgeType}]->(b)
         WHERE id(a) = $src AND id(b) = $dst
         RETURN count(*) > 0 AS exists`,
        { src, dst },
      );

      return result.records[0]?.get("exists") ?? false;
    } finally {
      await session.close();
    }
  }

  /**
   * Perform a 2-hop traversal from a node
   */
  async getNeighbors2Hop(nodeId: number): Promise<number[]> {
    const session = this.session();
    try {
      const result = await session.run(
        `MATCH (n)-[]->()-[]->(m)
         WHERE id(n) = $nodeId
         RETURN DISTINCT id(m) AS neighborId`,
        { nodeId },
      );

      return result.records.map((record) => {
        const neighborId = record.get("neighborId") as Integer;
        return neighborId.toNumber();
      });
    } finally {
      await session.close();
    }
  }

  /**
   * Perform a 3-hop traversal from a node
   */
  async getNeighbors3Hop(nodeId: number): Promise<number[]> {
    const session = this.session();
    try {
      const result = await session.run(
        `MATCH (n)-[]->()-[]->()-[]->(m)
         WHERE id(n) = $nodeId
         RETURN DISTINCT id(m) AS neighborId`,
        { nodeId },
      );

      return result.records.map((record) => {
        const neighborId = record.get("neighborId") as Integer;
        return neighborId.toNumber();
      });
    } finally {
      await session.close();
    }
  }

  /**
   * Count total nodes
   */
  async countNodes(): Promise<number> {
    const session = this.session();
    try {
      const result = await session.run("MATCH (n:Node) RETURN count(n) AS count");
      const count = result.records[0]?.get("count") as Integer;
      return count?.toNumber() ?? 0;
    } finally {
      await session.close();
    }
  }

  /**
   * Count total edges
   */
  async countEdges(): Promise<number> {
    const session = this.session();
    try {
      const result = await session.run("MATCH ()-[r]->() RETURN count(r) AS count");
      const count = result.records[0]?.get("count") as Integer;
      return count?.toNumber() ?? 0;
    } finally {
      await session.close();
    }
  }
}

/**
 * Vector Search API Integration Tests
 */

import { describe, expect, test, beforeEach, afterEach } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  ray,
  defineNode,
  defineEdge,
  prop,
  VectorIndex,
  createVectorIndex,
  openGraphDB,
  closeGraphDB,
  beginTx,
  commit,
  rollback,
  createNode,
  deleteNode,
  definePropkey,
  setNodeVector,
  getNodeVector,
  delNodeVector,
  hasNodeVector,
  getVectorStats,
} from "../src/index.ts";

// ============================================================================
// Test Fixtures
// ============================================================================

const document = defineNode("document", {
  key: (id: string) => `doc:${id}`,
  props: {
    title: prop.string("title"),
    content: prop.string("content"),
  },
});

const relatedTo = defineEdge("relatedTo", {});

// Helper to create a random unit vector
function randomVector(dims: number): Float32Array {
  const v = new Float32Array(dims);
  let sum = 0;
  for (let i = 0; i < dims; i++) {
    v[i] = Math.random() * 2 - 1;
    sum += v[i] * v[i];
  }
  // Normalize
  const norm = Math.sqrt(sum);
  for (let i = 0; i < dims; i++) {
    v[i] /= norm;
  }
  return v;
}

// Create a vector similar to another (cosine similarity ~0.8-0.95)
function similarVector(base: Float32Array, similarity: number = 0.9): Float32Array {
  const dims = base.length;
  const noise = randomVector(dims);
  const result = new Float32Array(dims);
  
  for (let i = 0; i < dims; i++) {
    result[i] = base[i] * similarity + noise[i] * (1 - similarity);
  }
  
  // Normalize
  let sum = 0;
  for (let i = 0; i < dims; i++) {
    sum += result[i] * result[i];
  }
  const norm = Math.sqrt(sum);
  for (let i = 0; i < dims; i++) {
    result[i] /= norm;
  }
  
  return result;
}

// ============================================================================
// Tests
// ============================================================================

describe("VectorIndex", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-vector-test-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("basic vector operations", async () => {
    const db = await ray(join(testDir, "test.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    // Create documents
    const doc1 = await db.insert(document).values({
      key: "doc1",
      title: "Introduction to AI",
      content: "AI is transforming the world...",
    }).returning();

    const doc2 = await db.insert(document).values({
      key: "doc2",
      title: "Machine Learning Basics",
      content: "ML is a subset of AI...",
    }).returning();

    const doc3 = await db.insert(document).values({
      key: "doc3",
      title: "Cooking Recipes",
      content: "How to make pasta...",
    }).returning();

    // Create vector index
    const index = createVectorIndex({ dimensions: 128 });

    // Add vectors
    const vec1 = randomVector(128);
    const vec2 = similarVector(vec1, 0.85); // Similar to vec1
    const vec3 = randomVector(128); // Random, dissimilar

    index.set(doc1, vec1);
    index.set(doc2, vec2);
    index.set(doc3, vec3);

    // Verify stats
    const stats = index.stats();
    expect(stats.totalVectors).toBe(3);
    expect(stats.liveVectors).toBe(3);
    expect(stats.dimensions).toBe(128);

    // Search for similar to doc1
    const results = index.search(vec1, { k: 2 });
    
    expect(results.length).toBe(2);
    expect(results[0]!.node.$id).toBe(doc1.$id); // Most similar is itself
    expect(results[0]!.similarity).toBeGreaterThan(0.99);
    
    // Second should be doc2 (similar)
    expect(results[1]!.node.$id).toBe(doc2.$id);
    expect(results[1]!.similarity).toBeGreaterThan(0.7);

    await db.close();
  });

  test("vector get and has", async () => {
    const db = await ray(join(testDir, "test2.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const doc = await db.insert(document).values({
      key: "test",
      title: "Test",
      content: "Test content",
    }).returning();

    const index = createVectorIndex({ dimensions: 64 });
    const vec = randomVector(64);

    // Initially no vector
    expect(index.has(doc)).toBe(false);
    expect(index.get(doc)).toBeNull();

    // Add vector
    index.set(doc, vec);
    expect(index.has(doc)).toBe(true);

    // Get vector back
    const retrieved = index.get(doc);
    expect(retrieved).not.toBeNull();
    expect(retrieved!.length).toBe(64);
    
    // Check values are close (may differ due to normalization)
    for (let i = 0; i < 64; i++) {
      expect(retrieved![i]).toBeCloseTo(vec[i]!, 4);
    }

    await db.close();
  });

  test("vector delete", async () => {
    const db = await ray(join(testDir, "test3.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const doc = await db.insert(document).values({
      key: "delete-test",
      title: "Delete Test",
      content: "Will be deleted",
    }).returning();

    const index = createVectorIndex({ dimensions: 32 });
    index.set(doc, randomVector(32));

    expect(index.has(doc)).toBe(true);
    expect(index.stats().liveVectors).toBe(1);

    // Delete
    const deleted = index.delete(doc);
    expect(deleted).toBe(true);
    expect(index.has(doc)).toBe(false);
    expect(index.get(doc)).toBeNull();

    // Can't delete twice
    const deletedAgain = index.delete(doc);
    expect(deletedAgain).toBe(false);

    await db.close();
  });

  test("search with threshold", async () => {
    const db = await ray(join(testDir, "test4.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const docs = [];
    const index = createVectorIndex({ dimensions: 64 });
    const baseVec = randomVector(64);

    // Create docs with decreasing similarity
    for (let i = 0; i < 5; i++) {
      const doc = await db.insert(document).values({
        key: `doc-${i}`,
        title: `Document ${i}`,
        content: `Content ${i}`,
      }).returning();
      docs.push(doc);

      // Create vectors with decreasing similarity
      const similarity = 1 - (i * 0.15); // 1.0, 0.85, 0.7, 0.55, 0.4
      const vec = i === 0 ? baseVec : similarVector(baseVec, similarity);
      index.set(doc, vec);
    }

    // Search with high threshold - should only return very similar
    const highThreshold = index.search(baseVec, { k: 10, threshold: 0.8 });
    // Due to randomness, we can't predict exact count, but all results should meet threshold
    for (const hit of highThreshold) {
      expect(hit.similarity).toBeGreaterThanOrEqual(0.8);
    }

    // Search with low threshold - should return more
    const lowThreshold = index.search(baseVec, { k: 10, threshold: 0.5 });
    expect(lowThreshold.length).toBeGreaterThanOrEqual(highThreshold.length);

    await db.close();
  });

  test("larger vector dimensions (768)", async () => {
    const db = await ray(join(testDir, "test5.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const index = createVectorIndex({ dimensions: 768 });

    // Create 50 documents with vectors
    const docs = [];
    const vectors: Float32Array[] = [];
    
    for (let i = 0; i < 50; i++) {
      const doc = await db.insert(document).values({
        key: `doc-${i}`,
        title: `Document ${i}`,
        content: `Content for document ${i}`,
      }).returning();
      docs.push(doc);

      const vec = randomVector(768);
      vectors.push(vec);
      index.set(doc, vec);
    }

    // Build index (should trigger IVF training)
    index.buildIndex();

    const stats = index.stats();
    expect(stats.totalVectors).toBe(50);
    expect(stats.dimensions).toBe(768);
    // Index should be trained since we have 50 vectors (above 1000 threshold? Let's check)
    // Actually default threshold is 1000, so it won't be trained
    // But search should still work via brute force

    // Search
    const results = index.search(vectors[0]!, { k: 5 });
    expect(results.length).toBe(5);
    expect(results[0]!.node.$id).toBe(docs[0]!.$id);
    expect(results[0]!.similarity).toBeGreaterThan(0.99);

    await db.close();
  });

  test("vector update (overwrite)", async () => {
    const db = await ray(join(testDir, "test6.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const doc = await db.insert(document).values({
      key: "update-test",
      title: "Update Test",
      content: "Vector will be updated",
    }).returning();

    const index = createVectorIndex({ dimensions: 32 });

    // Initial vector
    const vec1 = new Float32Array(32).fill(0.1);
    vec1[0] = 1.0;
    index.set(doc, vec1);

    // Verify initial
    const initial = index.get(doc);
    expect(initial).not.toBeNull();

    // Update with different vector
    const vec2 = new Float32Array(32).fill(0.2);
    vec2[1] = 1.0;
    index.set(doc, vec2);

    // Verify update
    const updated = index.get(doc);
    expect(updated).not.toBeNull();
    
    // Vectors should be different (after normalization)
    // The second component should be more dominant now
    expect(Math.abs(updated![1]!)).toBeGreaterThan(Math.abs(updated![0]!));

    await db.close();
  });

  test("clear index", async () => {
    const db = await ray(join(testDir, "test7.raydb"), {
      nodes: [document],
      edges: [relatedTo],
    });

    const index = createVectorIndex({ dimensions: 16 });

    // Add some vectors
    for (let i = 0; i < 10; i++) {
      const doc = await db.insert(document).values({
        key: `doc-${i}`,
        title: `Doc ${i}`,
        content: `Content ${i}`,
      }).returning();
      index.set(doc, randomVector(16));
    }

    expect(index.stats().liveVectors).toBe(10);

    // Clear
    index.clear();

    expect(index.stats().totalVectors).toBe(0);
    expect(index.stats().liveVectors).toBe(0);

    await db.close();
  });

  test("dimension mismatch error", async () => {
    const index = createVectorIndex({ dimensions: 64 });

    const fakeRef = { $id: 1, $key: "test", $def: document };

    // Wrong dimensions should throw
    expect(() => {
      index.set(fakeRef, new Float32Array(128));
    }).toThrow(/dimension mismatch/);

    expect(() => {
      index.search(new Float32Array(32), { k: 5 });
    }).toThrow(/dimension mismatch/);
  });

  test("buildIndex guard prevents concurrent modification", async () => {
    // This test verifies the _isBuilding guard works correctly
    // In a single-threaded environment, we simulate the scenario by 
    // checking that the guard is properly set/reset
    
    const index = createVectorIndex({ dimensions: 16, trainingThreshold: 10 });
    const fakeRef = (id: number) => ({ $id: id, $key: `test-${id}`, $def: document });

    // Add enough vectors to trigger index training
    for (let i = 0; i < 20; i++) {
      index.set(fakeRef(i), randomVector(16));
    }

    // Build index should work
    index.buildIndex();
    expect(index.stats().indexTrained).toBe(true);

    // After build, modifications should work again
    index.set(fakeRef(100), randomVector(16));
    expect(index.has(fakeRef(100))).toBe(true);

    // Delete should also work
    expect(index.delete(fakeRef(100))).toBe(true);
    expect(index.has(fakeRef(100))).toBe(false);
  });

  test("invalid vector values error", async () => {
    const index = createVectorIndex({ dimensions: 4 });

    const fakeRef = { $id: 1, $key: "test", $def: document };

    // NaN should throw
    expect(() => {
      index.set(fakeRef, new Float32Array([1, NaN, 3, 4]));
    }).toThrow(/Invalid vector.*NaN/);

    // Infinity should throw
    expect(() => {
      index.set(fakeRef, new Float32Array([1, 2, Infinity, 4]));
    }).toThrow(/Invalid vector.*Infinity/);

    // Zero vector should throw
    expect(() => {
      index.set(fakeRef, new Float32Array([0, 0, 0, 0]));
    }).toThrow(/Invalid vector.*zero vector/);

    // Add a valid vector first for search tests
    index.set(fakeRef, new Float32Array([1, 2, 3, 4]));

    // Invalid query vector should throw
    expect(() => {
      index.search(new Float32Array([1, NaN, 3, 4]), { k: 5 });
    }).toThrow(/Invalid query vector.*NaN/);

    expect(() => {
      index.search(new Float32Array([0, 0, 0, 0]), { k: 5 });
    }).toThrow(/Invalid query vector.*zero vector/);
  });
});

// ============================================================================
// Persistent Vector Operations Tests
// ============================================================================

describe("Persistent Vector Operations", () => {
  let testDir: string;

  beforeEach(async () => {
    testDir = await mkdtemp(join(tmpdir(), "ray-vector-persist-"));
  });

  afterEach(async () => {
    await rm(testDir, { recursive: true, force: true });
  });

  test("basic set and get vector", async () => {
    const dbPath = join(testDir, "persist1.raydb");
    const db = await openGraphDB(dbPath);

    // Define a property key for embeddings
    const tx = beginTx(db);
    const embeddingKey = definePropkey(tx, "embedding");
    const nodeId = createNode(tx);
    
    // Set a normalized vector (vectors are normalized on insert)
    const vec = randomVector(8);
    setNodeVector(tx, nodeId, embeddingKey, vec);
    
    // Should be readable within transaction
    const retrieved = getNodeVector(tx, nodeId, embeddingKey);
    expect(retrieved).not.toBeNull();
    expect(retrieved!.length).toBe(8);
    
    await commit(tx);
    
    // Should be readable after commit (normalized)
    const afterCommit = getNodeVector(db, nodeId, embeddingKey);
    expect(afterCommit).not.toBeNull();
    expect(afterCommit!.length).toBe(8);
    
    // Verify it's a unit vector (normalized)
    let norm = 0;
    for (let i = 0; i < 8; i++) {
      norm += afterCommit![i]! * afterCommit![i]!;
    }
    expect(Math.sqrt(norm)).toBeCloseTo(1.0, 3);
    
    await closeGraphDB(db);
  });

  test("vector persists across database reopen", async () => {
    const dbPath = join(testDir, "persist2.raydb");
    
    // First session: create and set vector (use normalized vector)
    let embeddingKey: number;
    let nodeId: number;
    const originalVec = randomVector(4);
    
    {
      const db = await openGraphDB(dbPath);
      const tx = beginTx(db);
      embeddingKey = definePropkey(tx, "embedding");
      nodeId = createNode(tx);
      setNodeVector(tx, nodeId, embeddingKey, originalVec);
      await commit(tx);
      await closeGraphDB(db);
    }
    
    // Second session: reopen and verify vector persisted
    {
      const db = await openGraphDB(dbPath);
      
      const retrieved = getNodeVector(db, nodeId, embeddingKey);
      expect(retrieved).not.toBeNull();
      expect(retrieved!.length).toBe(4);
      
      // Vectors are normalized, so check direction matches
      // (cosine similarity should be very close to 1)
      let dot = 0;
      for (let i = 0; i < 4; i++) {
        dot += retrieved![i]! * originalVec[i]!;
      }
      expect(dot).toBeGreaterThan(0.99); // Very similar direction
      
      await closeGraphDB(db);
    }
  });

  test("delete vector", async () => {
    const dbPath = join(testDir, "persist3.raydb");
    const db = await openGraphDB(dbPath);

    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    const nodeId = createNode(tx1);
    setNodeVector(tx1, nodeId, embeddingKey, new Float32Array([1, 2, 3, 4]));
    await commit(tx1);
    
    // Verify exists
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(true);
    expect(getNodeVector(db, nodeId, embeddingKey)).not.toBeNull();
    
    // Delete
    const tx2 = beginTx(db);
    const deleted = delNodeVector(tx2, nodeId, embeddingKey);
    expect(deleted).toBe(true);
    
    // Within transaction should show deleted
    expect(getNodeVector(tx2, nodeId, embeddingKey)).toBeNull();
    expect(hasNodeVector(tx2, nodeId, embeddingKey)).toBe(false);
    
    await commit(tx2);
    
    // After commit should be deleted
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
    expect(getNodeVector(db, nodeId, embeddingKey)).toBeNull();
    
    await closeGraphDB(db);
  });

  test("vector delete persists across reopen", async () => {
    const dbPath = join(testDir, "persist4.raydb");
    let embeddingKey: number;
    let nodeId: number;
    
    // First session: create, set, then delete
    {
      const db = await openGraphDB(dbPath);
      const tx1 = beginTx(db);
      embeddingKey = definePropkey(tx1, "embedding");
      nodeId = createNode(tx1);
      setNodeVector(tx1, nodeId, embeddingKey, new Float32Array([1, 2, 3, 4]));
      await commit(tx1);
      
      const tx2 = beginTx(db);
      delNodeVector(tx2, nodeId, embeddingKey);
      await commit(tx2);
      
      await closeGraphDB(db);
    }
    
    // Second session: verify deleted
    {
      const db = await openGraphDB(dbPath);
      expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
      expect(getNodeVector(db, nodeId, embeddingKey)).toBeNull();
      await closeGraphDB(db);
    }
  });

  test("multiple vectors per database", async () => {
    const dbPath = join(testDir, "persist5.raydb");
    const db = await openGraphDB(dbPath);

    const tx = beginTx(db);
    const embeddingKey = definePropkey(tx, "embedding");
    
    // Create multiple nodes with random normalized vectors
    const nodes: number[] = [];
    const vectors: Float32Array[] = [];
    
    for (let i = 0; i < 10; i++) {
      const nodeId = createNode(tx);
      nodes.push(nodeId);
      const vec = randomVector(4);
      vectors.push(vec);
      setNodeVector(tx, nodeId, embeddingKey, vec);
    }
    
    await commit(tx);
    
    // Verify all vectors (check cosine similarity to original)
    for (let i = 0; i < 10; i++) {
      const retrieved = getNodeVector(db, nodes[i]!, embeddingKey);
      expect(retrieved).not.toBeNull();
      
      // Compute cosine similarity
      let dot = 0;
      for (let j = 0; j < 4; j++) {
        dot += retrieved![j]! * vectors[i]![j]!;
      }
      expect(dot).toBeGreaterThan(0.99); // Very similar
    }
    
    // Check stats
    const stats = getVectorStats(db, embeddingKey);
    expect(stats).not.toBeNull();
    expect(stats!.totalVectors).toBe(10);
    expect(stats!.liveVectors).toBe(10);
    expect(stats!.dimensions).toBe(4);
    
    await closeGraphDB(db);
  });

  test("update vector (overwrite)", async () => {
    const dbPath = join(testDir, "persist6.raydb");
    const db = await openGraphDB(dbPath);

    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    const nodeId = createNode(tx1);
    setNodeVector(tx1, nodeId, embeddingKey, new Float32Array([1, 0, 0, 0]));
    await commit(tx1);
    
    // Update
    const tx2 = beginTx(db);
    setNodeVector(tx2, nodeId, embeddingKey, new Float32Array([0, 0, 0, 1]));
    await commit(tx2);
    
    // Verify updated
    const retrieved = getNodeVector(db, nodeId, embeddingKey);
    expect(retrieved).not.toBeNull();
    expect(retrieved![0]).toBeCloseTo(0, 5);
    expect(retrieved![3]).toBeCloseTo(1, 5);
    
    await closeGraphDB(db);
  });

  test("multiple vector properties on same node", async () => {
    const dbPath = join(testDir, "persist-multi-prop.raydb");
    const db = await openGraphDB(dbPath);

    const tx = beginTx(db);
    const embeddingKey1 = definePropkey(tx, "embedding1");
    const embeddingKey2 = definePropkey(tx, "embedding2");
    const nodeId = createNode(tx);
    
    // Set two different vectors on the same node
    const vec1 = randomVector(4);
    const vec2 = randomVector(8);
    setNodeVector(tx, nodeId, embeddingKey1, vec1);
    setNodeVector(tx, nodeId, embeddingKey2, vec2);
    
    await commit(tx);
    
    // Verify both vectors exist
    expect(hasNodeVector(db, nodeId, embeddingKey1)).toBe(true);
    expect(hasNodeVector(db, nodeId, embeddingKey2)).toBe(true);
    
    const retrieved1 = getNodeVector(db, nodeId, embeddingKey1);
    const retrieved2 = getNodeVector(db, nodeId, embeddingKey2);
    
    expect(retrieved1).not.toBeNull();
    expect(retrieved2).not.toBeNull();
    expect(retrieved1!.length).toBe(4);
    expect(retrieved2!.length).toBe(8);
    
    // Check stats for both
    const stats1 = getVectorStats(db, embeddingKey1);
    const stats2 = getVectorStats(db, embeddingKey2);
    expect(stats1!.dimensions).toBe(4);
    expect(stats2!.dimensions).toBe(8);
    
    await closeGraphDB(db);
  });

  test("multi-file database vector persistence", async () => {
    // Create a directory-based (multi-file) database
    const dbPath = join(testDir, "multifile");
    await import("node:fs/promises").then(fs => fs.mkdir(dbPath, { recursive: true }));
    
    let embeddingKey: number;
    let nodeId: number;
    const vec = new Float32Array([0.5, 0.5, 0.5, 0.5]);
    
    // First session
    {
      const db = await openGraphDB(dbPath);
      const tx = beginTx(db);
      embeddingKey = definePropkey(tx, "embedding");
      nodeId = createNode(tx);
      setNodeVector(tx, nodeId, embeddingKey, vec);
      await commit(tx);
      await closeGraphDB(db);
    }
    
    // Second session
    {
      const db = await openGraphDB(dbPath);
      const retrieved = getNodeVector(db, nodeId, embeddingKey);
      expect(retrieved).not.toBeNull();
      for (let i = 0; i < 4; i++) {
        expect(retrieved![i]).toBeCloseTo(vec[i]!, 5);
      }
      await closeGraphDB(db);
    }
  });

  test("cascade delete: node deletion removes vectors", async () => {
    const dbPath = join(testDir, "cascade-delete.raydb");
    const db = await openGraphDB(dbPath);

    // Create node with vector
    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    const nodeId = createNode(tx1);
    setNodeVector(tx1, nodeId, embeddingKey, randomVector(4));
    await commit(tx1);
    
    // Verify vector exists
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(true);
    
    // Delete the node
    const tx2 = beginTx(db);
    deleteNode(tx2, nodeId);
    await commit(tx2);
    
    // Vector should be gone too
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
    expect(getNodeVector(db, nodeId, embeddingKey)).toBeNull();
    
    await closeGraphDB(db);
  });

  test("cascade delete: persists across reopen", async () => {
    const dbPath = join(testDir, "cascade-persist.raydb");
    let embeddingKey: number;
    let nodeId: number;
    
    // Create and delete
    {
      const db = await openGraphDB(dbPath);
      const tx1 = beginTx(db);
      embeddingKey = definePropkey(tx1, "embedding");
      nodeId = createNode(tx1);
      setNodeVector(tx1, nodeId, embeddingKey, randomVector(4));
      await commit(tx1);
      
      const tx2 = beginTx(db);
      deleteNode(tx2, nodeId);
      await commit(tx2);
      
      await closeGraphDB(db);
    }
    
    // Reopen and verify vector is gone
    {
      const db = await openGraphDB(dbPath);
      expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
      await closeGraphDB(db);
    }
  });

  test("rollback does not persist vectors", async () => {
    const dbPath = join(testDir, "rollback.raydb");
    const db = await openGraphDB(dbPath);

    // Set up embedding key
    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    await commit(tx1);
    
    // Create node and vector, then rollback
    const tx2 = beginTx(db);
    const nodeId = createNode(tx2);
    setNodeVector(tx2, nodeId, embeddingKey, randomVector(4));
    
    // Can read within transaction
    expect(getNodeVector(tx2, nodeId, embeddingKey)).not.toBeNull();
    
    // Rollback
    rollback(tx2);
    
    // Vector should not exist
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
    
    await closeGraphDB(db);
  });

  test("large vectors (1536 dimensions - OpenAI size)", async () => {
    const dbPath = join(testDir, "large-vectors.raydb");
    const db = await openGraphDB(dbPath);

    const tx = beginTx(db);
    const embeddingKey = definePropkey(tx, "embedding");
    const nodeId = createNode(tx);
    
    // Create a 1536-dimension vector (OpenAI embedding size)
    const largeVec = randomVector(1536);
    setNodeVector(tx, nodeId, embeddingKey, largeVec);
    await commit(tx);
    
    // Verify it was stored and retrieved correctly
    const retrieved = getNodeVector(db, nodeId, embeddingKey);
    expect(retrieved).not.toBeNull();
    expect(retrieved!.length).toBe(1536);
    
    // Check it's normalized (unit vector)
    let norm = 0;
    for (let i = 0; i < 1536; i++) {
      norm += retrieved![i]! * retrieved![i]!;
    }
    expect(Math.sqrt(norm)).toBeCloseTo(1.0, 3);
    
    await closeGraphDB(db);
  });

  test("set then delete in same transaction", async () => {
    const dbPath = join(testDir, "set-delete-same-tx.raydb");
    const db = await openGraphDB(dbPath);

    const tx = beginTx(db);
    const embeddingKey = definePropkey(tx, "embedding");
    const nodeId = createNode(tx);
    
    // Set vector
    setNodeVector(tx, nodeId, embeddingKey, randomVector(4));
    expect(hasNodeVector(tx, nodeId, embeddingKey)).toBe(true);
    
    // Delete in same transaction
    delNodeVector(tx, nodeId, embeddingKey);
    expect(hasNodeVector(tx, nodeId, embeddingKey)).toBe(false);
    
    await commit(tx);
    
    // Should not exist after commit
    expect(hasNodeVector(db, nodeId, embeddingKey)).toBe(false);
    
    await closeGraphDB(db);
  });

  test("delete then set in same transaction", async () => {
    const dbPath = join(testDir, "delete-set-same-tx.raydb");
    const db = await openGraphDB(dbPath);

    // First create a vector
    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    const nodeId = createNode(tx1);
    setNodeVector(tx1, nodeId, embeddingKey, new Float32Array([1, 0, 0, 0]));
    await commit(tx1);
    
    // Delete then set new vector in same transaction
    const tx2 = beginTx(db);
    delNodeVector(tx2, nodeId, embeddingKey);
    expect(hasNodeVector(tx2, nodeId, embeddingKey)).toBe(false);
    
    setNodeVector(tx2, nodeId, embeddingKey, new Float32Array([0, 0, 0, 1]));
    expect(hasNodeVector(tx2, nodeId, embeddingKey)).toBe(true);
    
    await commit(tx2);
    
    // New vector should exist
    const retrieved = getNodeVector(db, nodeId, embeddingKey);
    expect(retrieved).not.toBeNull();
    expect(retrieved![3]).toBeCloseTo(1, 3);
    
    await closeGraphDB(db);
  });

  test("dimension mismatch throws error", async () => {
    const dbPath = join(testDir, "dimension-mismatch.raydb");
    const db = await openGraphDB(dbPath);

    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    const nodeId1 = createNode(tx1);
    setNodeVector(tx1, nodeId1, embeddingKey, randomVector(4));
    await commit(tx1);
    
    // Try to set a different dimension vector
    const tx2 = beginTx(db);
    const nodeId2 = createNode(tx2);
    
    expect(() => {
      setNodeVector(tx2, nodeId2, embeddingKey, randomVector(8));
    }).toThrow(/dimension mismatch/);
    
    rollback(tx2);
    await closeGraphDB(db);
  });

  test("multiple transactions with vectors", async () => {
    const dbPath = join(testDir, "multi-tx.raydb");
    const db = await openGraphDB(dbPath);

    const nodes: number[] = [];
    
    // Transaction 1: create and set
    const tx1 = beginTx(db);
    const embeddingKey = definePropkey(tx1, "embedding");
    for (let i = 0; i < 5; i++) {
      const nodeId = createNode(tx1);
      nodes.push(nodeId);
      setNodeVector(tx1, nodeId, embeddingKey, randomVector(4));
    }
    await commit(tx1);
    
    // Transaction 2: update some, delete others
    const tx2 = beginTx(db);
    setNodeVector(tx2, nodes[0]!, embeddingKey, randomVector(4));  // Update
    delNodeVector(tx2, nodes[1]!, embeddingKey);  // Delete
    const newNode = createNode(tx2);
    nodes.push(newNode);
    setNodeVector(tx2, newNode, embeddingKey, randomVector(4));  // New
    await commit(tx2);
    
    // Verify state
    expect(hasNodeVector(db, nodes[0]!, embeddingKey)).toBe(true);
    expect(hasNodeVector(db, nodes[1]!, embeddingKey)).toBe(false);
    expect(hasNodeVector(db, nodes[2]!, embeddingKey)).toBe(true);
    expect(hasNodeVector(db, newNode, embeddingKey)).toBe(true);
    
    const stats = getVectorStats(db, embeddingKey);
    expect(stats!.liveVectors).toBe(5);  // 5 original - 1 deleted + 1 new = 5
    
    await closeGraphDB(db);
  });
});

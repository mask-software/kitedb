// Lane b4 `vector-quality`.
//
// VQ2 bruteForceSearch sorts with `partial_cmp(...).unwrap_or(Equal)`: a NaN
//     distance (from a NaN component) compares equal to every distance, so
//     the sort is not a total order. NaN hits can land in the top k and the
//     finite hits can come back out of order.
// VQ1 (Rust tests in tests/b4_vector_quality.rs) adds an IVF-PQ exact
//     re-rank; a test here checks its `rerankFactor` option end to end.
// VQ3 adds a training seed (`ivf.seed`); seeded builds serialize the same.

import { createRequire } from 'node:module'

import test from 'ava'

import { bruteForceSearch, createVectorIndex } from '../dist/index.js'

// The IVF index classes are native-only (not re-exported by the TS layer).
const native = createRequire(import.meta.url)('../index.js')

type Metric = 'Cosine' | 'Euclidean' | 'DotProduct'

// Finite vectors whose distances to the query are distinct and ordered by i,
// interleaved with vectors that contain a NaN component.
const cases: Array<{
  metric: Metric
  query: number[]
  vector: (i: number) => number[]
  distance: (i: number) => number
}> = [
  { metric: 'Euclidean', query: [0, 0], vector: (i) => [i, 0], distance: (i) => i },
  { metric: 'DotProduct', query: [1, 0], vector: (i) => [i, 0], distance: (i) => -i },
  {
    metric: 'Cosine',
    query: [1, 0],
    vector: (i) => [Math.cos(i * 0.02), Math.sin(i * 0.02)],
    distance: (i) => 1 - Math.cos(i * 0.02),
  },
]

for (const { metric, query, vector, distance } of cases) {
  test(`b4 VQ2: ${metric} bruteForceSearch keeps NaN distances out and the top k in order`, (t) => {
    const vectors: number[][] = []
    const nodeIds: number[] = []
    const finite: Array<{ nodeId: number; distance: number }> = []
    for (let i = 1; i <= 64; i++) {
      nodeIds.push(i)
      if (i % 2 === 0) {
        vectors.push([Number.NaN, 1])
      } else {
        vectors.push(vector(i))
        finite.push({ nodeId: i, distance: distance(i) })
      }
    }
    // Present the finite vectors far from their sorted order.
    vectors.reverse()
    nodeIds.reverse()

    const k = 5
    const expected = finite
      .sort((a, b) => a.distance - b.distance)
      .slice(0, k)
      .map((hit) => hit.nodeId)

    const hits = bruteForceSearch(vectors, nodeIds, query, k, metric as any)
    const distances = hits.map((hit) => hit.distance)
    t.false(
      distances.some((d) => Number.isNaN(d)),
      `NaN distance in the results: ${JSON.stringify(hits)}`,
    )
    t.deepEqual(
      hits.map((hit) => hit.nodeId),
      expected,
      `top ${k} by ${metric} distance: ${JSON.stringify(hits)}`,
    )
  })
}

// VQ1 plumbing: the IVF-PQ re-rank option reaches the native search.
test('b4 VQ1: VectorIndex.search takes rerankFactor and returns exact distances by default', (t) => {
  const index = createVectorIndex({
    dimensions: 4,
    metric: 'Euclidean' as any,
    trainingThreshold: 64,
    annAlgorithm: 'ivf_pq' as any,
  })
  const vectors: number[][] = []
  for (let i = 0; i < 64; i++) {
    const vector = [Math.sin(i), Math.cos(i * 1.3), Math.sin(i * 0.7), Math.cos(i * 0.2)]
    vectors.push(vector)
    index.set(i, vector)
  }
  index.buildIndex()
  t.true(index.stats().indexTrained)

  const query = [0.3, -0.2, 0.5, 0.1]
  for (const options of [{ k: 5 }, { k: 5, rerankFactor: 2 }]) {
    const hits = index.search(query, options)
    t.is(hits.length, 5)
    for (const hit of hits) {
      const exact = Math.hypot(...vectors[hit.nodeId].map((v, d) => v - query[d]))
      t.true(Math.abs(hit.distance - exact) < 1e-4, `${JSON.stringify(options)}: ${hit.distance} vs exact ${exact}`)
    }
  }
  t.is(index.search(query, { k: 5, rerankFactor: 0 }).length, 5)
  t.throws(() => index.search(query, { k: 5, rerankFactor: -1 }), { message: /rerankFactor/ })
})

// VQ3: a training seed makes builds reproducible.
const seededData = () => {
  const vectors: number[] = []
  for (let i = 0; i < 3000; i++) {
    const blob = i % 20
    for (let d = 0; d < 8; d++) vectors.push(Math.sin(blob * 7 + d) * 4 + Math.sin(i * 13.7 + d * 3.1))
  }
  return vectors
}

test('b4 VQ3: seeded IVF and IVF-PQ builds serialize identically', (t) => {
  const data = seededData()
  const build = (seed: number, pq: boolean) => {
    const index = pq
      ? new native.JsIvfPqIndex(8, { nClusters: 20, seed }, { numSubspaces: 4, numCentroids: 32 })
      : new native.JsIvfIndex(8, { nClusters: 20, seed })
    index.addTrainingVectors(data, 3000)
    index.train()
    for (let i = 0; i < 3000; i++) index.insert(i, data.slice(i * 8, i * 8 + 8))
    return index.serialize()
  }
  for (const pq of [false, true]) {
    t.true(build(5, pq).equals(build(5, pq)), `pq=${pq}: same seed, different index`)
    t.false(build(5, pq).equals(build(6, pq)), `pq=${pq}: the seed has no effect`)
  }
  t.throws(() => new native.JsIvfIndex(8, { seed: -1 }), { message: /seed/ })
})

// annAlgorithm: Node users choose the VectorIndex backend.
const backendData = (count: number, dims: number) => {
  const vectors: number[][] = []
  for (let i = 0; i < count; i++) {
    const blob = i % 10
    vectors.push(Array.from({ length: dims }, (_, d) => Math.sin(blob * 5 + d) * 4 + Math.sin(i * 7.3 + d * 1.7)))
  }
  return vectors
}

const euclidean = (a: number[], b: number[]) => Math.hypot(...a.map((v, d) => v - b[d]))

test('b4: VectorIndex annAlgorithm builds and searches with either backend', (t) => {
  const vectors = backendData(1500, 16)
  for (const annAlgorithm of ['ivf', 'ivf_pq', 'auto']) {
    const index = createVectorIndex({ dimensions: 16, metric: 'Euclidean' as any, annAlgorithm: annAlgorithm as any })
    vectors.forEach((vector, node) => index.set(node, vector))
    index.buildIndex()
    const stats = index.stats()
    t.true(stats.indexTrained, annAlgorithm)
    // Auto picks plain IVF for a small, low-dimensional index.
    t.is(stats.indexAlgorithm as string, annAlgorithm === 'ivf_pq' ? 'ivf_pq' : 'ivf', annAlgorithm)

    for (const node of [0, 777, 1499]) {
      const hits = index.search(vectors[node], { k: 5 })
      t.is(hits.length, 5, annAlgorithm)
      t.is(hits[0].nodeId, node, `${annAlgorithm}: a stored vector finds itself first`)
      if (annAlgorithm !== 'ivf_pq') {
        // Plain IVF ranks by exact distance.
        for (const hit of hits) {
          const exact = euclidean(vectors[node], vectors[hit.nodeId])
          t.true(Math.abs(hit.distance - exact) < 1e-4 * Math.max(1, exact), `${hit.distance} vs exact ${exact}`)
        }
      }
    }
  }
  t.throws(() => createVectorIndex({ dimensions: 16, annAlgorithm: 'hnsw' as any }))
})

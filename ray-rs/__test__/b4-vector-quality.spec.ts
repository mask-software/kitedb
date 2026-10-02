// Lane b4 `vector-quality`.
//
// VQ2 bruteForceSearch sorts with `partial_cmp(...).unwrap_or(Equal)`: a NaN
//     distance (from a NaN component) compares equal to every distance, so
//     the sort is not a total order. NaN hits can land in the top k and the
//     finite hits can come back out of order.
// VQ1 (Rust tests in tests/b4_vector_quality.rs) adds an IVF-PQ exact
//     re-rank; the last test checks its `rerankFactor` option end to end.

import test from 'ava'

import { bruteForceSearch, createVectorIndex } from '../dist/index.js'

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
  const index = createVectorIndex({ dimensions: 4, metric: 'Euclidean' as any, trainingThreshold: 64 })
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

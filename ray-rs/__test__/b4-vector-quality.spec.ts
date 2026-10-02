// Lane b4 `vector-quality`.
//
// VQ2 bruteForceSearch sorts with `partial_cmp(...).unwrap_or(Equal)`: a NaN
//     distance (from a NaN component) compares equal to every distance, so
//     the sort is not a total order. NaN hits can land in the top k and the
//     finite hits can come back out of order.

import test from 'ava'

import { bruteForceSearch } from '../dist/index.js'

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

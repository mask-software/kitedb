import { edge, node, kite } from '../dist'

const user = node('user', {
  props: {
    name: { type: 'string' },
    email: { type: 'string' },
  },
})

const follows = edge('follows', {
  props: {
    type: 'int',
  },
})

// Define schema inline when opening the database
const db = await kite('./social.kitedb', {
  nodes: [user],
  edges: [follows],
})

type NodeView = { id: number; key: string }

type EdgeView = {
  src: number
  etype: number
  dst: number
  etypeName: string
}

const logEl = document.querySelector('#log') as HTMLPreElement | null
const statusEl = document.querySelector('#status') as HTMLSpanElement | null
const addNodeBtn = document.querySelector('#add-node') as HTMLButtonElement | null
const addEdgeBtn = document.querySelector('#add-edge') as HTMLButtonElement | null
const graphEl = document.querySelector('#graph') as SVGSVGElement | null
const selectedNodeEl = document.querySelector('#selected-node') as HTMLDivElement | null
const connectionsOutEl = document.querySelector('#connections-out') as HTMLUListElement | null
const connectionsInEl = document.querySelector('#connections-in') as HTMLUListElement | null

const log = (message: string) => {
  if (!logEl) return
  logEl.textContent += message + '\n'
  logEl.scrollTop = logEl.scrollHeight
}

const DB_PATH = '/demo.raydb'
const EDGE_TYPE = 'connects'

const state = {
  ray: null as any,
  db: null as any,
  nodes: [] as NodeView[],
  edges: [] as EdgeView[],
  positions: new Map<number, { x: number; y: number }>(),
  selectedNodeId: null as number | null,
  nextSeed: 1,
}

const setStatus = (text: string) => {
  if (statusEl) statusEl.textContent = `WASM: ${text}`
}

const getNodeKey = (id: number) => {
  const node = state.nodes.find((n) => n.id === id)
  return node ? node.key : `node-${id}`
}

const refreshState = () => {
  if (!state.db) return
  const db = state.db
  const nodes = db.listNodes() as number[]
  const edges = db.listEdges() as Array<{ src: number; etype: number; dst: number }>
  const nodeViews = nodes.map((id) => ({ id, key: db.getNodeKey(id) ?? `node-${id}` }))

  const etypeId = db.getEtypeId(EDGE_TYPE)
  const etypeName = etypeId !== null ? db.getEtypeName(etypeId) ?? EDGE_TYPE : EDGE_TYPE

  state.nodes = nodeViews
  state.edges = edges.map((edge) => ({
    ...edge,
    etypeName,
  }))
}

const renderGraph = () => {
  if (!graphEl) return
  if (state.nodes.length === 0) {
    graphEl.innerHTML = '<text x="400" y="260" text-anchor="middle" fill="#64748b">No nodes yet</text>'
    return
  }

  const width = 800
  const height = 520
  const radius = Math.min(width, height) / 2 - 80
  const centerX = width / 2
  const centerY = height / 2
  const positions = new Map<number, { x: number; y: number }>()

  state.nodes.forEach((node, index) => {
    const angle = (index / state.nodes.length) * Math.PI * 2 - Math.PI / 2
    const x = centerX + radius * Math.cos(angle)
    const y = centerY + radius * Math.sin(angle)
    positions.set(node.id, { x, y })
  })

  state.positions = positions

  const activeEdges = new Set<string>()
  if (state.selectedNodeId !== null) {
    state.edges.forEach((edge) => {
      if (edge.src === state.selectedNodeId || edge.dst === state.selectedNodeId) {
        activeEdges.add(`${edge.src}:${edge.dst}`)
      }
    })
  }

  const edgeMarkup = state.edges
    .map((edge) => {
      const srcPos = positions.get(edge.src)
      const dstPos = positions.get(edge.dst)
      if (!srcPos || !dstPos) return ''
      const active = activeEdges.has(`${edge.src}:${edge.dst}`) ? 'active' : ''
      return `<line class="edge-line ${active}" x1="${srcPos.x}" y1="${srcPos.y}" x2="${dstPos.x}" y2="${dstPos.y}" marker-end="url(#arrow)" />`
    })
    .join('')

  const neighborIds = new Set<number>()
  if (state.selectedNodeId !== null) {
    state.edges.forEach((edge) => {
      if (edge.src === state.selectedNodeId) neighborIds.add(edge.dst)
      if (edge.dst === state.selectedNodeId) neighborIds.add(edge.src)
    })
  }

  const nodeMarkup = state.nodes
    .map((node) => {
      const pos = positions.get(node.id)
      if (!pos) return ''
      const isSelected = node.id === state.selectedNodeId
      const isNeighbor = neighborIds.has(node.id)
      const classes = [
        'node-circle',
        isSelected ? 'selected' : '',
        !isSelected && isNeighbor ? 'neighbor' : '',
      ]
        .filter(Boolean)
        .join(' ')
      return `
      <g data-node-id="${node.id}">
        <circle class="${classes}" cx="${pos.x}" cy="${pos.y}" r="22"></circle>
        <text class="node-label" x="${pos.x}" y="${pos.y + 4}" text-anchor="middle">${node.key}</text>
      </g>
      `
    })
    .join('')

  graphEl.innerHTML = `
    <defs>
      <marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
        <path d="M 0 0 L 10 5 L 0 10 z" fill="rgba(15, 23, 42, 0.6)"></path>
      </marker>
    </defs>
    ${edgeMarkup}
    ${nodeMarkup}
  `
}

const renderConnections = () => {
  if (!connectionsOutEl || !connectionsInEl || !selectedNodeEl) return
  if (state.selectedNodeId === null) {
    selectedNodeEl.textContent = 'None'
    connectionsOutEl.innerHTML = ''
    connectionsInEl.innerHTML = ''
    return
  }

  const selectedKey = getNodeKey(state.selectedNodeId)
  selectedNodeEl.textContent = `${selectedKey} (#${state.selectedNodeId})`

  const out = state.edges.filter((edge) => edge.src === state.selectedNodeId)
  const inc = state.edges.filter((edge) => edge.dst === state.selectedNodeId)

  connectionsOutEl.innerHTML = out.length
    ? out.map((edge) => `<li>${getNodeKey(edge.dst)} (#${edge.dst})</li>`).join('')
    : '<li>None</li>'

  connectionsInEl.innerHTML = inc.length
    ? inc.map((edge) => `<li>${getNodeKey(edge.src)} (#${edge.src})</li>`).join('')
    : '<li>None</li>'
}

const updateView = () => {
  refreshState()
  renderGraph()
  renderConnections()
}

const addRandomNode = () => {
  if (!state.db) return
  const stamp = Date.now().toString(36)
  const key = `node-${state.nextSeed}-${stamp}`
  state.nextSeed += 1
  const db = state.db
  db.begin()
  try {
    db.createNode(key)
    db.commit()
    log(`Added node ${key}.`)
  } catch (err) {
    db.rollback()
    throw err
  }
  updateView()
}

const addRandomEdge = () => {
  if (!state.db) return
  if (state.nodes.length < 2) {
    log('Add at least two nodes first.')
    return
  }

  const db = state.db
  const etype = db.getOrCreateEtype(EDGE_TYPE)

  const pickNodeId = () => state.nodes[Math.floor(Math.random() * state.nodes.length)].id

  let src = state.selectedNodeId ?? pickNodeId()
  let dst = pickNodeId()
  let attempts = 0
  while (dst === src && attempts < 5) {
    dst = pickNodeId()
    attempts += 1
  }

  db.begin()
  try {
    db.addEdge(src, etype, dst)
    db.commit()
    log(`Added edge ${getNodeKey(src)} -> ${getNodeKey(dst)}.`)
  } catch (err) {
    db.rollback()
    throw err
  }
  updateView()
}

const bindEvents = () => {
  addNodeBtn?.addEventListener('click', () => addRandomNode())
  addEdgeBtn?.addEventListener('click', () => addRandomEdge())

  graphEl?.addEventListener('click', (event) => {
    const target = event.target as HTMLElement | null
    const group = target?.closest('g[data-node-id]') as SVGGElement | null
    if (!group) return
    const id = Number(group.dataset.nodeId)
    if (Number.isNaN(id)) return
    state.selectedNodeId = id
    renderGraph()
    renderConnections()
  })
}

const init = async () => {
  setStatus('loading')
  log('Loading WASM...')

  const ray = (await import('../../../core.wasi-browser.js')) as any
  state.ray = ray
  state.db = ray.Database.open(DB_PATH)
  state.db.begin()
  try {
    state.db.getOrCreateEtype(EDGE_TYPE)
    state.db.commit()
  } catch (err) {
    state.db.rollback()
    throw err
  }

  setStatus('ready')
  log('Database ready.')
  updateView()
}

bindEvents()
void init().catch((err) => {
  log(err?.stack ?? String(err))
  setStatus('error')
})

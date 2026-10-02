# RayDB Playground - Implementation Plan

A web-based graph visualization tool for exploring code graphs stored in RayDB. Elysia.js backend serves both the API and the React frontend.

## Decisions

- **File loading**: Path input + file upload with size limit (10MB), clearly communicated to user
- **Graph limit**: 1000 nodes max, warn user if truncated
- **Build**: `Bun.build()` for React, serve from `dist/`

---

## Directory Structure

```
playground/
├── package.json
├── build.ts                       # Build script for client
├── src/
│   ├── server.ts                  # Elysia server entry point
│   ├── api/
│   │   ├── routes.ts              # API route definitions
│   │   ├── db.ts                  # DB connection manager (singleton)
│   │   └── demo-data.ts           # Demo code graph generator
│   └── client/
│       ├── index.html             # HTML shell
│       ├── index.tsx              # React entry
│       ├── app.tsx                # Main app
│       ├── styles.css             # Global styles
│       ├── components/
│       │   ├── graph-canvas.tsx
│       │   ├── header.tsx
│       │   ├── toolbar.tsx
│       │   ├── sidebar.tsx
│       │   └── status-bar.tsx
│       ├── hooks/
│       │   ├── use-graph-data.ts
│       │   └── use-simulation.ts
│       └── lib/
│           ├── api.ts
│           └── types.ts
└── dist/                          # Built client output (gitignored)
```

---

## Implementation Phases

### Phase 1: Backend Foundation

1. **`src/api/db.ts`** - Database manager singleton
   - `openDatabase(path: string)` - Opens a `.kitedb` file
   - `openFromBuffer(buffer: Uint8Array)` - Opens from uploaded file
   - `createDemo()` - Creates demo database
   - `close()` - Closes current database
   - `getDb()` - Returns current database instance or null
   - Track current DB path for status

2. **`src/api/demo-data.ts`** - Demo code graph
   - ~30 nodes: files, functions, classes
   - ~50 edges: imports, calls, contains
   - Realistic code structure (e.g., a mini web server)

3. **`src/api/routes.ts`** - All API endpoints
   - Database management (open/close/demo/status)
   - Node CRUD
   - Edge CRUD
   - Graph network endpoint (returns D3-ready format)
   - Path finding
   - Impact analysis

4. **`src/server.ts`** - Elysia server
   - Mount API routes
   - Serve static files from `dist/`
   - File upload endpoint with 10MB limit
   - CORS for development

### Phase 2: Client Foundation

5. **`src/client/index.html`** - Minimal HTML shell

6. **`src/client/lib/types.ts`** - TypeScript types
   - `GraphNode`, `GraphEdge`, `GraphNetwork`
   - `PathResult`, `ImpactResult`
   - API response types

7. **`src/client/lib/api.ts`** - API client
   - Fetch wrappers for all endpoints
   - Error handling

8. **`build.ts`** - Build script
   - Uses `Bun.build()` to bundle React app
   - Outputs to `dist/`

### Phase 3: UI Components

9. **`src/client/app.tsx`** - Main layout
   - Header, Toolbar, Canvas, Sidebar, StatusBar
   - Global state (selected node, tool mode, etc.)

10. **`src/client/components/header.tsx`**
    - Logo/title
    - Search input (filters nodes)
    - Path mode inputs (From → To)
    - Database selector (path input, file upload, demo button)

11. **`src/client/components/toolbar.tsx`**
    - Select mode (pointer icon)
    - Path mode (route icon)
    - Impact mode (zap icon)
    - Zoom in/out/reset buttons

12. **`src/client/components/sidebar.tsx`**
    - Selected node details (label, type, properties)
    - Connected nodes list
    - Path results (when in path mode)
    - Impact results (when in impact mode)

13. **`src/client/components/status-bar.tsx`**
    - Node/edge counts
    - Zoom level
    - FPS
    - DB path / status
    - Truncation warning if applicable

### Phase 4: Graph Visualization

14. **`src/client/hooks/use-simulation.ts`**
    - D3 force simulation setup
    - Physics config (charge, link distance, center)
    - Start/stop/reheat controls

15. **`src/client/components/graph-canvas.tsx`**
    - Canvas rendering loop
    - Draw nodes (circles with labels)
    - Draw edges (lines)
    - Highlight selected/hovered/path nodes
    - Mouse handlers (pan, zoom, select, drag)

### Phase 5: Analysis Features

16. **`src/client/hooks/use-graph-data.ts`**
    - Fetch graph network
    - Search/filter nodes
    - Handle loading/error states

17. **Path finding integration**
    - UI to select start/end nodes
    - Call API, highlight result path

18. **Impact analysis integration**
    - UI to trigger from selected node
    - Call API, highlight impacted nodes

### Phase 6: Polish

19. **Styling** - Dark theme matching reference
20. **Error handling** - User-friendly error messages
21. **Loading states** - Spinners/skeletons
22. **Keyboard shortcuts** - P for path, I for impact, Esc to clear, etc.

---

## API Specification

```typescript
// Database Management
GET  /api/status              → { connected: boolean, path?: string, nodeCount?: number, edgeCount?: number }
GET  /api/replication/status  → { connected: boolean, role: "primary"|"replica"|"disabled", primary?: ..., replica?: ... }
GET  /api/replication/metrics → text/plain (Prometheus exposition format)
GET  /api/replication/snapshot/latest → { success: boolean, snapshot?: { byteLength, sha256, ... } }
GET  /api/replication/log?cursor=...&maxBytes=...&maxFrames=... → { success: boolean, frames: [...], nextCursor, eof }
POST /api/replication/pull    ← { maxFrames?: number } → { success: boolean, appliedFrames?: number, replica?: ... }
POST /api/replication/reseed  → { success: boolean, replica?: ... }
POST /api/replication/promote → { success: boolean, epoch?: number, primary?: ... }
POST /api/db/open             ← { path: string, options?: { readOnly?, syncMode?, replicationRole?, ... } } → { success: boolean, error?: string }
POST /api/db/upload           ← FormData (file) → { success: boolean, error?: string }
POST /api/db/demo             → { success: boolean }
POST /api/db/close            → { success: boolean }

// Stats
GET  /api/stats               → { nodes: number, edges: number, ... }

// Nodes
GET  /api/nodes               → { nodes: Node[], truncated: boolean }
GET  /api/nodes/:id           → Node | 404
POST /api/nodes               ← { key: string, type?: string, props?: Record<string, any> } → Node
DELETE /api/nodes/:id         → { success: boolean }

// Edges
GET  /api/edges               → { edges: Edge[], truncated: boolean }
POST /api/edges               ← { src: string, type: string, dst: string } → { success: boolean }
DELETE /api/edges             ← { src: string, type: string, dst: string } → { success: boolean }

// Visualization
GET  /api/graph/network       → { nodes: VisNode[], edges: VisEdge[], truncated: boolean }

// Analysis
POST /api/graph/path          ← { startKey: string, endKey: string } → { path: string[], edges: string[] } | { error: string }
POST /api/graph/impact        ← { nodeKey: string } → { impacted: string[], edges: string[] }
```

Replication admin auth:
- Auth mode envs:
  - `REPLICATION_ADMIN_AUTH_MODE` = `none|token|mtls|token_or_mtls|token_and_mtls`
  - `REPLICATION_ADMIN_TOKEN` for token modes
  - `REPLICATION_MTLS_HEADER` (default `x-forwarded-client-cert`) for mTLS modes
  - `REPLICATION_MTLS_SUBJECT_REGEX` optional subject filter for mTLS modes
  - `REPLICATION_MTLS_NATIVE_TLS=true` to treat native HTTPS + client-cert verification as mTLS auth
  - `PLAYGROUND_TLS_CERT_FILE` + `PLAYGROUND_TLS_KEY_FILE` enable HTTPS listener
  - `PLAYGROUND_TLS_CA_FILE` optional custom client-cert CA bundle
  - `PLAYGROUND_TLS_REQUEST_CERT` + `PLAYGROUND_TLS_REJECT_UNAUTHORIZED` for TLS client-cert enforcement
- Admin endpoints (`/snapshot/latest`, `/metrics`, `/log`, `/pull`, `/reseed`, `/promote`) enforce the selected mode.
- `/api/replication/status` remains readable without auth.

---

## Node/Edge Visualization Format

```typescript
interface VisNode {
  id: string;        // node key (e.g., "file:src/index.ts")
  label: string;     // display label (e.g., "index.ts")
  type: string;      // node type (file, function, class, etc.)
  color?: string;    // optional color based on type
  degree: number;    // connection count (for sizing)
}

interface VisEdge {
  source: string;    // source node id
  target: string;    // target node id
  type: string;      // edge type (imports, calls, etc.)
}
```

---

## Color Scheme

```typescript
const COLORS = {
  bg: "#05080f",
  surface: "#0c1222",
  border: "rgba(230, 190, 138, 0.1)",
  accent: "#e6be8a",
  textMain: "#e0e6ed",
  textMuted: "#94a3b8",

  // Node types
  file: "#3B82F6",      // blue
  function: "#e6be8a",  // gold
  class: "#22C55E",     // green
  module: "#A855F7",    // purple

  // Highlights
  selected: "#6b8bb2",
  pathStart: "#22C55E",
  pathEnd: "#EF4444",
  pathNode: "#4ADE80",
  impact: "#F59E0B",
};
```

---

## Demo Data Schema

```typescript
// Node definitions
const FileNode = defineNode("file", (path: string) => `file:${path}`, {
  path: prop.string(),
  language: prop.string(),
});

const FunctionNode = defineNode("function", (name: string) => `fn:${name}`, {
  name: prop.string(),
  file: prop.string(),
  line: optional(prop.number()),
});

const ClassNode = defineNode("class", (name: string) => `class:${name}`, {
  name: prop.string(),
  file: prop.string(),
});

// Edge definitions
const ImportsEdge = defineEdge("imports", {});      // file → file
const CallsEdge = defineEdge("calls", {});          // function → function
const ContainsEdge = defineEdge("contains", {});    // file → function, class → method
const ExtendsEdge = defineEdge("extends", {});      // class → class
```

# Cytoscape.js Migration Plan

Migration from D3-force + HTML5 Canvas to Cytoscape.js for graph visualization.

## Current State

- **D3-force** (`d3-force`) for physics-based force-directed layout
- **HTML5 Canvas API** for manual rendering
- Custom hit-testing, zoom handling, and drawing logic in `graph-canvas.tsx`
- Simulation hook (`use-simulation.ts`) manages D3 force simulation

## Target State

- **Cytoscape.js** with **fCoSE** layout (fast force-directed, high quality for code graphs)
- Cytoscape manages viewport internally
- Bezier edges with automatic bundling
- Bird's eye navigator widget

---

## User Requirements

| Requirement | Decision |
|-------------|----------|
| Layout algorithm | fCoSE (best for code graphs) |
| Viewport management | Cytoscape internal (Option A - simpler) |
| Node dragging | Only in "select" mode (disabled in path/impact modes) |
| Extensions | `cytoscape-fcose`, `cytoscape-popper`, `cytoscape-navigator` |
| Migration scope | Complete replacement (no D3 fallback) |
| Tooltips | Sidebar sufficient (no Tippy.js) |
| Navigator | Yes, bird's eye view component |
| Layout animation | Animate nodes into place smoothly |
| Edge rendering | Bezier curves with bundling |

---

## Implementation Phases

### Phase 1: Dependencies & Basic Setup

1. **Update `package.json`**
   - Add: `cytoscape`, `cytoscape-fcose`, `cytoscape-popper`, `@popperjs/core`, `cytoscape-navigator`
   - Remove: `d3`, `@types/d3`

2. **Create `lib/cytoscape-theme.ts`**
   - Base stylesheet matching Ray Electric Blue theme
   - Node styles by type
   - Edge styles (bezier)
   - Selection/hover glow effects
   - Path/impact highlight classes

3. **Create `hooks/use-cytoscape.ts`**
   - Initialize Cytoscape instance with container ref
   - Configure fCoSE layout options
   - Element update methods
   - Cleanup on unmount

4. **Update `lib/types.ts`**
   - Keep `VisNode`, `VisEdge` interfaces
   - Remove D3 simulation properties (`SimNode`, `SimLink`, `vx`, `vy`, etc.)
   - Add Cytoscape element conversion types if needed

### Phase 2: Core Graph Component

1. **Replace `graph-canvas.tsx` internals**
   - Remove Canvas element, add container div for Cytoscape
   - Remove manual drawing loop
   - Remove hit-testing logic

2. **Data conversion**
   - `VisNode[]` → Cytoscape node elements
   - `VisEdge[]` → Cytoscape edge elements
   - Handle incremental updates

3. **Configure fCoSE layout**
   ```typescript
   {
     name: 'fcose',
     quality: 'proof',
     animate: true,
     animationDuration: 500,
     fit: true,
     padding: 50,
     nodeDimensionsIncludeLabels: true,
     idealEdgeLength: 100,
     nodeRepulsion: 4500,
     edgeElasticity: 0.45,
   }
   ```

4. **Base styles**
   - Node colors by type
   - Edge styles (bezier curves)
   - Labels (positioned below nodes)
   - Selection state

### Phase 3: Interaction Handling

1. **Event wiring**
   - `tap` → `onNodeClick`
   - `mouseover`/`mouseout` → `onNodeHover`
   - `tapunselect` → clear selection

2. **Conditional dragging**
   - Use `autoungrabify: true` in path/impact modes
   - Use `autoungrabify: false` in select mode

3. **Toolbar integration**
   - Zoom in: `cy.zoom(cy.zoom() * 1.2)`
   - Zoom out: `cy.zoom(cy.zoom() / 1.2)`
   - Fit: `cy.fit(50)`
   - Center: `cy.center()`

4. **Viewport management**
   - Remove zoom/pan from React state
   - Let Cytoscape manage internally
   - Remove manual transform calculations

### Phase 4: Feature Parity

1. **Path highlighting**
   - Add `.path-node` class to nodes in path
   - Add `.path-edge` class to edges in path
   - Style with gold color (`#FFD700`)

2. **Impact analysis highlighting**
   - Add `.impacted` class to affected nodes
   - Style with red color (`#FF6B6B`)

3. **Selection & hover styles**
   - `:selected` selector with glow
   - `.hovered` class with subtle highlight

4. **Node sizing**
   - Size based on degree using `mapData()`
   - `width: mapData(degree, 0, 20, 30, 60)`

5. **Label visibility**
   - Use `min-zoomed-font-size` for automatic hiding at low zoom
   - Or toggle based on zoom level

6. **Navigator component**
   - Create separate component for bird's eye view
   - Position in corner of graph area
   - Wire to Cytoscape instance

### Phase 5: Cleanup

1. **Remove D3**
   - Delete `d3` from `package.json`
   - Delete `@types/d3` from devDependencies
   - Run `bun install`

2. **Delete unused files**
   - `hooks/use-simulation.ts`

3. **Clean up types**
   - Remove any remaining D3 references from `types.ts`

4. **Clean up app state**
   - Remove viewport state from `app.tsx` if still present
   - Simplify zoom/pan handlers

---

## Theme Colors (Ray Electric Blue)

```typescript
const COLORS = {
  // Base
  background: '#0D1117',
  surface: '#161B22',
  border: 'rgba(0, 229, 255, 0.2)',
  
  // Accent
  accent: '#00E5FF',
  accentDim: 'rgba(0, 229, 255, 0.5)',
  
  // Text
  textPrimary: '#E6EDF3',
  textSecondary: '#8B949E',
  
  // Node types
  file: '#3B82F6',      // blue
  function: '#00E5FF',  // cyan
  class: '#22C55E',     // green
  module: '#A855F7',    // purple
  
  // Highlights
  selected: '#00E5FF',
  hovered: 'rgba(0, 229, 255, 0.3)',
  pathHighlight: '#FFD700',  // gold
  impactHighlight: '#FF6B6B', // red
  
  // Edges
  edgeDefault: 'rgba(139, 148, 158, 0.6)',
  edgeHighlight: '#00E5FF',
};
```

---

## Cytoscape Stylesheet Preview

```typescript
const stylesheet: cytoscape.Stylesheet[] = [
  // Base node style
  {
    selector: 'node',
    style: {
      'background-color': '#3B82F6',
      'label': 'data(label)',
      'color': '#E6EDF3',
      'text-valign': 'bottom',
      'text-margin-y': 8,
      'font-size': 12,
      'min-zoomed-font-size': 8,
      'width': 'mapData(degree, 0, 20, 30, 60)',
      'height': 'mapData(degree, 0, 20, 30, 60)',
    },
  },
  
  // Node types
  { selector: 'node[type="file"]', style: { 'background-color': '#3B82F6' } },
  { selector: 'node[type="function"]', style: { 'background-color': '#00E5FF' } },
  { selector: 'node[type="class"]', style: { 'background-color': '#22C55E' } },
  { selector: 'node[type="module"]', style: { 'background-color': '#A855F7' } },
  
  // Base edge style
  {
    selector: 'edge',
    style: {
      'width': 1.5,
      'line-color': 'rgba(139, 148, 158, 0.6)',
      'target-arrow-color': 'rgba(139, 148, 158, 0.6)',
      'target-arrow-shape': 'triangle',
      'curve-style': 'bezier',
      'arrow-scale': 0.8,
    },
  },
  
  // Selection
  {
    selector: 'node:selected',
    style: {
      'border-width': 3,
      'border-color': '#00E5FF',
      'box-shadow': '0 0 15px #00E5FF',
    },
  },
  
  // Path highlighting
  {
    selector: '.path-node',
    style: {
      'background-color': '#FFD700',
      'border-width': 2,
      'border-color': '#FFD700',
    },
  },
  {
    selector: '.path-edge',
    style: {
      'line-color': '#FFD700',
      'target-arrow-color': '#FFD700',
      'width': 3,
    },
  },
  
  // Impact highlighting
  {
    selector: '.impacted',
    style: {
      'background-color': '#FF6B6B',
      'border-width': 2,
      'border-color': '#FF6B6B',
    },
  },
];
```

---

## Files to Modify

| File | Action |
|------|--------|
| `package.json` | Update dependencies |
| `lib/types.ts` | Remove D3 types |
| `lib/cytoscape-theme.ts` | **Create** - Stylesheet |
| `hooks/use-cytoscape.ts` | **Create** - Hook |
| `hooks/use-simulation.ts` | **Delete** |
| `components/graph-canvas.tsx` | Rewrite for Cytoscape |
| `app.tsx` | Remove viewport state |

---

## Testing Checklist

- [ ] Graph renders with demo data
- [ ] Nodes colored by type
- [ ] Node sizing reflects degree
- [ ] Edges render as bezier curves
- [ ] Click to select node
- [ ] Hover shows highlight
- [ ] Drag nodes (select mode only)
- [ ] Drag disabled in path/impact modes
- [ ] Zoom in/out/fit buttons work
- [ ] Path highlighting works
- [ ] Impact highlighting works
- [ ] Navigator shows bird's eye view
- [ ] Layout animates smoothly
- [ ] No D3 references remain

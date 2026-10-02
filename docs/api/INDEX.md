# Kite API Documentation Index

Welcome to the Kite High-Level API documentation! This index will help you find what you need.

## Documentation Files

### [QUICK_START.md](./QUICK_START.md) - **Start Here! (5 minutes)**
Perfect for getting your first database running.

**Contents:**
- Basic setup and installation
- Common operations (CRUD)
- Graph traversal examples
- Common patterns (many-to-many, hierarchical, time-indexed)
- Troubleshooting
- Type inference guide

**Best for:** First-time users, quick reference

---

### [README.md](./README.md) - **Complete Reference (Bookmark This)**
The most comprehensive documentation of the API.

**Contents:**
- Overview of all five modules
- Key concepts (schema-first, type inference)
- Full module reference
- Method signatures and parameters
- Advanced patterns
- Error handling
- Performance tips
- API layer comparison

**Sections:**
- **Module Reference** - Detailed docs for each module
  - `kite.ts` - Database and operations
  - `schema.ts` - Schema definition
  - `builders.ts` - Query builders
  - `traversal.ts` - Graph traversal
- **Advanced Patterns** - Transactions, batching, properties, types
- **Performance Tips** - Optimization strategies
- **Comparison** - High-level vs low-level API

**Best for:** Complete understanding, API reference

---

### [ARCHITECTURE.md](./ARCHITECTURE.md) - **Design Deep Dive**
For understanding how the API is built and why.

**Contents:**
- Design philosophy
- Core concepts breakdown
- Data flow for each operation
- Property type system
- Design patterns used
- Transaction model
- Error handling strategy
- Performance characteristics
- Comparison with other databases
- Future work and roadmap

**Best for:** Maintainers, framework builders, deep understanding

---

### [../API.md](../API.md) - **Project Architecture**
High-level overview of the entire Kite project.

**Contents:**
- Full system architecture
- All layers (API, DB, Storage, Utils)
- File structure
- When to use which API
- Common patterns
- Type inference examples

**Best for:** Project overview, choosing the right API

---

## How to Navigate

### I want to...

**Get started quickly**
→ Read [QUICK_START.md](./QUICK_START.md) (5 min)
→ Try the examples

**Understand a specific feature**
→ Use [README.md](./README.md) - search for the feature
→ Look for code examples

**Build something specific**
→ Check [QUICK_START.md](./QUICK_START.md) common patterns
→ Refer to [README.md](./README.md) for detailed signatures

**Understand the design**
→ Read [ARCHITECTURE.md](./ARCHITECTURE.md)
→ Look at [../API.md](../API.md) for broader context

**Learn about type inference**
→ See Type Inference section in [README.md](./README.md)
→ Check schema examples in [QUICK_START.md](./QUICK_START.md)

**Fix a problem**
→ Check "Troubleshooting" in [QUICK_START.md](./QUICK_START.md)
→ See "Error Handling" in [README.md](./README.md)

**Optimize performance**
→ See "Performance Tips" in [README.md](./README.md)
→ Check [ARCHITECTURE.md](./ARCHITECTURE.md) for characteristics

**Use advanced features**
→ See "Advanced Patterns" in [README.md](./README.md)
→ Read [ARCHITECTURE.md](./ARCHITECTURE.md) for how they work

---

## Reading Order

### Path 1: Get Started Immediately
1. [QUICK_START.md](./QUICK_START.md) - Set up and run basic example
2. [README.md](./README.md) - Look up specific features as needed
3. [ARCHITECTURE.md](./ARCHITECTURE.md) - Understand how it works (optional)

### Path 2: Understand Everything
1. [../API.md](../API.md) - Project overview
2. [QUICK_START.md](./QUICK_START.md) - Practical basics
3. [README.md](./README.md) - Complete reference
4. [ARCHITECTURE.md](./ARCHITECTURE.md) - Design details

### Path 3: Build a Specific Feature
1. [QUICK_START.md](./QUICK_START.md) - Find similar pattern
2. [README.md](./README.md) - Look up exact API
3. [README.md](./README.md) Advanced Patterns - Complex cases
4. Source code - Fine details

---

## Quick Reference

### Schema Definition
- Property types: `string()`, `int()`, `float()`, `bool()` (aliases: `prop.string()`, `prop.int()`, ...)
- Optional: `optional(X())` (or `optional(prop.X())`)
- Nodes: `defineNode(name, { key, props })`
- Edges: `defineEdge(name, props?)`

### CRUD Operations
- Insert: `db.insert(nodeType).values({...}).returning()`
- Get: `db.get(nodeType, key)`
- Update: `db.update(nodeType, key).setAll({...}).execute()`
- Delete: `db.delete(nodeType, key)`

### Relationships
- Create: `db.link(src, edgeType, dst, props?)`
- Delete: `db.unlink(src, edgeType, dst)`
- Check: `db.hasEdge(src, edgeType, dst)`

### Traversal
- Out: `db.from(node).out(edge).nodes().toArray()`
- In: `db.from(node).in(edge).nodes().toArray()`
- Both: `db.from(node).both(edge).nodes().toArray()`
- Multi-hop: `db.from(node).out(e1).out(e2).nodes().toArray()`
- Variable depth: `db.from(node).traverse(edge, { direction: 'out', maxDepth: 3 })`

### Transactions
- Single: `db.transaction(async (ctx) => { ... })`
- Batch: `db.batch([op1, op2, op3])`

### Types
- Insert: `InferNodeInsert<typeof nodeType>`
- Return: `InferNode<typeof nodeType>`
- Edge props: `InferEdgeProps<typeof edgeType>`

---

## Tips

**For Large Documents:**
- Use your editor's search (Ctrl/Cmd+F) to find sections
- Jump to specific method names in [README.md](./README.md)
- Use section headers to navigate

**For Learning:**
- Start with examples in [QUICK_START.md](./QUICK_START.md)
- Run the code to see it work
- Refer to [README.md](./README.md) for details
- Check source code JSDoc for in-editor help

**For Reference:**
- Bookmark [README.md](./README.md)
- Use search for method names
- Look at similar examples for patterns

**For Troubleshooting:**
- Check "Error Handling" in [README.md](./README.md)
- See "Troubleshooting" in [QUICK_START.md](./QUICK_START.md)
- Review the "Error Handling Strategy" in [ARCHITECTURE.md](./ARCHITECTURE.md)

---

## Documentation Highlights

### Comprehensive Examples
Every major feature has copy-paste ready examples:
- Insert/update/delete patterns
- Single and bulk operations
- Relationship management
- Complex traversals
- Transaction safety

### Type Safety Emphasis
Full coverage of:
- Property type system
- Type inference
- Type errors and solutions
- Generic type parameters

### Performance Guidance
Includes:
- Operation complexity
- Optimization strategies
- When to call `optimize()`

### Real-World Patterns
Examples for:
- User relationships (social graphs)
- Organizational hierarchies
- Time-indexed events
- Many-to-many relationships

---

## Related Files

**Source Code:**
- `kite.ts` - Main database class
- `schema.ts` - Schema builders
- `builders.ts` - Query builders
- `traversal.ts` - Traversal implementation
- `index.ts` - Public exports

**Project Level:**
- `../../README.md` - Main project documentation
- `../API.md` - Full architecture overview

**Lower-Level APIs:**
- `src/db/graph-db.ts` - Low-level database (use via `$raw` escape hatch)
- `src/core/` - Storage layer (internal)
- `src/util/` - Utilities (internal)

---

## Questions?

If documentation doesn't answer your question:

1. **Check the examples** - Most questions are answered by examples
2. **Search [README.md](./README.md)** - Comprehensive reference
3. **Look at source JSDoc** - Enhanced comments in code files
4. **Read [ARCHITECTURE.md](./ARCHITECTURE.md)** - Understand design
5. **Check tests** - `tests/` directory has integration examples

---

**Last Updated:** December 14, 2025

**Documentation Includes:**
- Complete API reference
- Getting started guide
- 50+ code examples
- Architecture explanation
- Type inference guide
- Performance tips
- Error handling
- Common patterns
- Troubleshooting

**Version:** 1.0

import {
  Database,
  type DbStats,
  type JsNodeProp,
  type JsPropValue,
  type JsFullEdge,
  type NodePage,
  type EdgePage,
} from "@kitedb/core";

export interface NamedProp {
  key: string;
  value: string;
}

export interface NodeDetail {
  id: number;
  key: string | null;
  props: NamedProp[];
  labels: string[];
  outDegree: number;
  inDegree: number;
  outEdges: EdgeRef[];
  inEdges: EdgeRef[];
}

export interface EdgeRef {
  src: number;
  etype: number;
  etypeName: string;
  dst: number;
}

export interface EdgeDetail {
  src: number;
  etype: number;
  etypeName: string;
  dst: number;
  props: NamedProp[];
}

export interface PageState<T> {
  items: T[];
  cursor?: string;
  nextCursor?: string;
  hasMore: boolean;
  total?: number;
}

export interface PageRequest {
  limit: number;
  cursor?: string;
}

/**
 * How many schema ids to probe when listing labels or edge types. Database can only name an id,
 * not list them. Ids start at 1 and may have holes (a rolled-back define burns its id), so probe a
 * fixed range instead of stopping at the first miss.
 */
const MAX_SCHEMA_ID = 1024;

class SizedCache<K, V> {
  private map = new Map<K, V>();
  constructor(private maxSize: number) {}

  get(key: K): V | undefined {
    const value = this.map.get(key);
    if (value !== undefined) {
      this.map.delete(key);
      this.map.set(key, value);
    }
    return value;
  }

  set(key: K, value: V) {
    if (this.map.has(key)) {
      this.map.delete(key);
    }
    this.map.set(key, value);
    if (this.map.size > this.maxSize) {
      const firstKey = this.map.keys().next().value as K;
      this.map.delete(firstKey);
    }
  }

  clear() {
    this.map.clear();
  }
}

export class DbService {
  private db: Database | null = null;
  private readOnly = true;
  private nodeKeyCache = new SizedCache<number, string>(5000);
  private etypeNameCache = new SizedCache<number, string>(2000);
  private propKeyNameCache = new SizedCache<number, string>(5000);
  private labelNameCache = new SizedCache<number, string>(2000);

  isOpen() {
    return this.db !== null && this.db.isOpen;
  }

  getPath() {
    return this.db?.path ?? null;
  }

  isReadOnly() {
    return this.readOnly;
  }

  open(path: string, readOnly: boolean) {
    this.close();
    this.db = Database.open(path, { readOnly });
    this.readOnly = readOnly;
    this.clearCaches();
  }

  close() {
    if (this.db) {
      this.db.close();
    }
    this.db = null;
    this.readOnly = true;
    this.clearCaches();
  }

  stats(): DbStats | null {
    if (!this.db) return null;
    return this.db.stats();
  }

  getLabels(): string[] {
    if (!this.db) return [];
    return probeSchemaNames((id) => this.getLabelName(id));
  }

  getEdgeTypes(): string[] {
    if (!this.db) return [];
    return probeSchemaNames((id) => this.getEtypeName(id));
  }

  getNodesPage(req: PageRequest): PageState<number> {
    if (!this.db) return { items: [], hasMore: false };
    const page: NodePage = this.db.getNodesPage({ limit: req.limit, cursor: req.cursor });
    return {
      items: page.items,
      cursor: req.cursor,
      nextCursor: page.nextCursor,
      hasMore: page.hasMore,
      total: page.total,
    };
  }

  getEdgesPage(req: PageRequest): PageState<JsFullEdge> {
    if (!this.db) return { items: [], hasMore: false };
    const page: EdgePage = this.db.getEdgesPage({ limit: req.limit, cursor: req.cursor });
    return {
      items: page.items,
      cursor: req.cursor,
      nextCursor: page.nextCursor,
      hasMore: page.hasMore,
      total: page.total,
    };
  }

  getNodeKey(nodeId: number): string | null {
    if (!this.db) return null;
    const cached = this.nodeKeyCache.get(nodeId);
    if (cached !== undefined) return cached;
    const key = this.db.getNodeKey(nodeId);
    // Misses aren't cached: a missing node can be created later (by an import, or another writer).
    if (key !== null) this.nodeKeyCache.set(nodeId, key);
    return key;
  }

  getNodeLabels(nodeId: number): string[] {
    if (!this.db) return [];
    const labelIds = this.db.getNodeLabels(nodeId);
    return labelIds.map((id) => this.getLabelName(id) ?? `#${id}`);
  }

  getNodeProps(nodeId: number): NamedProp[] {
    if (!this.db) return [];
    const props = this.db.getNodeProps(nodeId) ?? [];
    return props.map((prop) => ({
      key: this.getPropKeyName(prop.keyId) ?? `#${prop.keyId}`,
      value: formatPropValue(prop.value),
    }));
  }

  getEdgeProps(src: number, etype: number, dst: number): NamedProp[] {
    if (!this.db) return [];
    const props = this.db.getEdgeProps(src, etype, dst) ?? [];
    return props.map((prop) => ({
      key: this.getPropKeyName(prop.keyId) ?? `#${prop.keyId}`,
      value: formatPropValue(prop.value),
    }));
  }

  getNodeDetail(nodeId: number): NodeDetail | null {
    if (!this.db) return null;
    const key = this.getNodeKey(nodeId);
    const props = this.getNodeProps(nodeId);
    const labels = this.getNodeLabels(nodeId);
    // getOutEdges/getInEdges give the other endpoint as `nodeId`.
    const outEdges = this.db.getOutEdges(nodeId).map((edge) => ({
      src: nodeId,
      etype: edge.etype,
      etypeName: this.getEtypeName(edge.etype) ?? `#${edge.etype}`,
      dst: edge.nodeId,
    }));
    const inEdges = this.db.getInEdges(nodeId).map((edge) => ({
      src: edge.nodeId,
      etype: edge.etype,
      etypeName: this.getEtypeName(edge.etype) ?? `#${edge.etype}`,
      dst: nodeId,
    }));

    return {
      id: nodeId,
      key,
      props,
      labels,
      outDegree: this.db.getOutDegree(nodeId),
      inDegree: this.db.getInDegree(nodeId),
      outEdges,
      inEdges,
    };
  }

  getEdgeDetail(edge: JsFullEdge): EdgeDetail | null {
    if (!this.db) return null;
    return {
      src: edge.src,
      etype: edge.etype,
      etypeName: this.getEtypeName(edge.etype) ?? `#${edge.etype}`,
      dst: edge.dst,
      props: this.getEdgeProps(edge.src, edge.etype, edge.dst),
    };
  }

  exportJson(path: string) {
    if (!this.db) throw new Error("No database open");
    return this.db.exportToJson(path);
  }

  exportJsonl(path: string) {
    if (!this.db) throw new Error("No database open");
    return this.db.exportToJsonl(path);
  }

  importJson(path: string) {
    if (!this.db) throw new Error("No database open");
    try {
      return this.db.importFromJson(path);
    } finally {
      // Even a failed import may have written some nodes and schema names.
      this.clearCaches();
    }
  }

  resolveEdgeTypeFilter(filter: string): number | null {
    if (!this.db) return null;
    const trimmed = filter.trim();
    if (!trimmed) return null;
    const numeric = Number(trimmed);
    if (!Number.isNaN(numeric)) return numeric;
    return this.db.getEtypeId(trimmed);
  }

  getEdgeTypeName(etype: number): string | null {
    return this.getEtypeName(etype);
  }

  private getEtypeName(etype: number): string | null {
    if (!this.db) return null;
    const cached = this.etypeNameCache.get(etype);
    if (cached !== undefined) return cached;
    const name = this.db.getEtypeName(etype);
    if (name) this.etypeNameCache.set(etype, name);
    return name;
  }

  private getPropKeyName(keyId: number): string | null {
    if (!this.db) return null;
    const cached = this.propKeyNameCache.get(keyId);
    if (cached !== undefined) return cached;
    const name = this.db.getPropkeyName(keyId);
    if (name) this.propKeyNameCache.set(keyId, name);
    return name;
  }

  private getLabelName(labelId: number): string | null {
    if (!this.db) return null;
    const cached = this.labelNameCache.get(labelId);
    if (cached !== undefined) return cached;
    const name = this.db.getLabelName(labelId);
    if (name) this.labelNameCache.set(labelId, name);
    return name;
  }

  private clearCaches() {
    this.nodeKeyCache.clear();
    this.etypeNameCache.clear();
    this.propKeyNameCache.clear();
    this.labelNameCache.clear();
  }
}

function probeSchemaNames(nameOf: (id: number) => string | null): string[] {
  const names: string[] = [];
  for (let id = 1; id <= MAX_SCHEMA_ID; id++) {
    const name = nameOf(id);
    if (name) names.push(name);
  }
  return names;
}

export function formatPropValue(value: JsPropValue): string {
  const propType = value.propType as unknown as string | number;
  switch (propType) {
    case "Null":
    case 0:
      return "null";
    case "Bool":
    case 1:
      return String(value.boolValue ?? false);
    case "Int":
    case 2:
      return String(value.intValue ?? 0);
    case "Float":
    case 3:
      return String(value.floatValue ?? 0);
    case "String":
    case 4:
      return value.stringValue ?? "";
    case "Vector":
    case 5:
      return `vector(${value.vectorValue?.length ?? 0})`;
    default:
      return "";
  }
}

export function formatProps(props: JsNodeProp[], getName: (keyId: number) => string | null): NamedProp[] {
  return props.map((prop) => ({
    key: getName(prop.keyId) ?? `#${prop.keyId}`,
    value: formatPropValue(prop.value),
  }));
}

export function formatEdge(edge: JsFullEdge, getEtypeName: (etype: number) => string | null): EdgeRef {
  return {
    src: edge.src,
    etype: edge.etype,
    etypeName: getEtypeName(edge.etype) ?? `#${edge.etype}`,
    dst: edge.dst,
  };
}

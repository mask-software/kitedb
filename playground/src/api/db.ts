/**
 * Database Manager Singleton
 *
 * Manages the current KiteDB connection for the playground.
 */

import { tmpdir } from "node:os";
import { basename, dirname, join, resolve, sep } from "node:path";
import {
	type Kite,
	type KiteOptions,
	defineEdge,
	defineNode,
	kite,
	optional,
	prop,
} from "../../../ray-rs/ts/index.ts";
import { createDemoGraph } from "./demo-data.ts";

import {
	lstat,
	mkdir,
	mkdtemp,
	realpath,
	rm,
	writeFile,
} from "node:fs/promises";

// ============================================================================
// Schema Definitions (shared with demo data)
// ============================================================================

export const FileNode = defineNode("file", {
	key: (path: string) => `file:${path}`,
	props: {
		path: prop.string("path"),
		language: prop.string("language"),
	},
});

export const FunctionNode = defineNode("function", {
	key: (name: string) => `fn:${name}`,
	props: {
		name: prop.string("name"),
		file: prop.string("file"),
		line: optional(prop.int("line")),
	},
});

export const ClassNode = defineNode("class", {
	key: (name: string) => `class:${name}`,
	props: {
		name: prop.string("name"),
		file: prop.string("file"),
	},
});

export const ModuleNode = defineNode("module", {
	key: (name: string) => `module:${name}`,
	props: {
		name: prop.string("name"),
	},
});

export const ImportsEdge = defineEdge("imports");
export const CallsEdge = defineEdge("calls");
export const ContainsEdge = defineEdge("contains");
export const ExtendsEdge = defineEdge("extends");

export const nodes = [FileNode, FunctionNode, ClassNode, ModuleNode];
export const edges = [ImportsEdge, CallsEdge, ContainsEdge, ExtendsEdge];

// ============================================================================
// Data Directory
// ============================================================================

/** Default for PLAYGROUND_DATA_DIR: `playground/data`. */
const DEFAULT_DATA_DIR = resolve(import.meta.dir, "../../data");

/** Open options that name filesystem paths; confined like the database path. */
const PATH_OPTIONS = [
	"replicationSidecarPath",
	"replicationSourceDbPath",
	"replicationSourceSidecarPath",
] as const;

/** Uploads are stored under a fixed name; the client-supplied filename is never used. */
const UPLOAD_FILE_NAME = "upload.kitedb";

/**
 * The only directory client-supplied paths may point into.
 * Read on every call so PLAYGROUND_DATA_DIR changes take effect without a restart.
 */
export function getDataDir(): string {
	const configured = process.env.PLAYGROUND_DATA_DIR?.trim();
	return resolve(configured || DEFAULT_DATA_DIR);
}

/**
 * Resolve symlinks in the longest existing prefix of `path`; the missing tail is kept as-is.
 */
async function canonicalPath(path: string): Promise<string> {
	const missing: string[] = [];
	let current = path;
	for (;;) {
		try {
			return join(await realpath(current), ...missing);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
				throw error;
			}
			// realpath fails on a dangling symlink, but creating the file would follow it.
			const isDanglingLink = await lstat(current).then(
				() => true,
				() => false,
			);
			if (isDanglingLink) {
				throw new Error(`Refusing to follow dangling symlink: ${current}`);
			}
			const parent = dirname(current);
			if (parent === current) {
				return path;
			}
			missing.unshift(basename(current));
			current = parent;
		}
	}
}

/**
 * Resolve a client-supplied path against the data directory.
 * Relative paths are taken relative to it; anything that resolves outside it
 * (absolute paths elsewhere, `..` escapes, symlinks pointing out) is rejected.
 */
export async function resolveDataPath(input: string): Promise<string> {
	const dataDir = getDataDir();
	await mkdir(dataDir, { recursive: true });
	const root = await realpath(dataDir);
	const target = await canonicalPath(resolve(root, input));
	const prefix = root.endsWith(sep) ? root : root + sep;
	if (!target.startsWith(prefix)) {
		throw new Error(
			`Path must be inside the playground data directory (${dataDir}): ${input}`,
		);
	}
	return target;
}

/**
 * Run `fn` with a fresh temp directory, removing the directory if `fn` throws.
 * tmpdir() is read per call so TMPDIR changes take effect.
 */
async function withTempDir<T>(
	prefix: string,
	fn: (dir: string) => Promise<T>,
): Promise<T> {
	const dir = await mkdtemp(join(tmpdir(), prefix));
	try {
		return await fn(dir);
	} catch (error) {
		await rm(dir, { recursive: true, force: true });
		throw error;
	}
}

// ============================================================================
// Database Manager
// ============================================================================

interface DbState {
	db: Kite;
	path: string;
	isDemo: boolean;
	tempDir?: string;
}

let currentDb: DbState | null = null;

export type PlaygroundOpenOptions = Omit<KiteOptions, "nodes" | "edges">;

/**
 * Open a database from a client-supplied path.
 * The path and any path-valued options must resolve inside the data directory.
 */
export async function openDatabase(
	path: string,
	options?: PlaygroundOpenOptions,
): Promise<{ success: boolean; error?: string }> {
	try {
		const dbPath = await resolveDataPath(path);
		const openOptions: PlaygroundOpenOptions = { ...options };
		for (const key of PATH_OPTIONS) {
			const value = openOptions[key];
			if (value !== undefined) {
				openOptions[key] = await resolveDataPath(value);
			}
		}

		await closeDatabase();

		const db = await kite(dbPath, { nodes, edges, ...openOptions });
		currentDb = { db, path: dbPath, isDemo: false };

		return { success: true };
	} catch (error) {
		return {
			success: false,
			error: error instanceof Error ? error.message : "Failed to open database",
		};
	}
}

/**
 * Open a database from an uploaded buffer, copied into a private temp directory
 */
export async function openFromBuffer(
	buffer: Uint8Array,
): Promise<{ success: boolean; error?: string }> {
	try {
		await closeDatabase();

		currentDb = await withTempDir("kitedb-playground-", async (tempDir) => {
			const tempPath = join(tempDir, UPLOAD_FILE_NAME);
			await writeFile(tempPath, buffer);
			const db = await kite(tempPath, { nodes, edges });
			return { db, path: tempPath, isDemo: false, tempDir };
		});

		return { success: true };
	} catch (error) {
		return {
			success: false,
			error: error instanceof Error ? error.message : "Failed to open database",
		};
	}
}

/**
 * Create and open a demo database
 */
export async function createDemo(): Promise<{
	success: boolean;
	error?: string;
}> {
	try {
		await closeDatabase();

		currentDb = await withTempDir("kitedb-demo-", async (tempDir) => {
			const demoPath = join(tempDir, "demo.kitedb");
			const db = await kite(demoPath, { nodes, edges });
			try {
				await createDemoGraph(db);
			} catch (error) {
				await db.close();
				throw error;
			}
			return { db, path: demoPath, isDemo: true, tempDir };
		});

		return { success: true };
	} catch (error) {
		return {
			success: false,
			error:
				error instanceof Error
					? error.message
					: "Failed to create demo database",
		};
	}
}

/**
 * Close the current database
 */
export async function closeDatabase(): Promise<{ success: boolean }> {
	if (currentDb) {
		try {
			await currentDb.db.close();

			// Clean up temp directory if it exists
			if (currentDb.tempDir) {
				await rm(currentDb.tempDir, { recursive: true, force: true });
			}
		} catch {
			// Ignore close errors
		}
		currentDb = null;
	}
	return { success: true };
}

/**
 * Get the current database instance
 */
export function getDb(): Kite | null {
	return currentDb?.db ?? null;
}

/**
 * Get the current database path
 */
export function getDbPath(): string | null {
	return currentDb?.path ?? null;
}

/**
 * Check if the current database is the demo
 */
export function isDemo(): boolean {
	return currentDb?.isDemo ?? false;
}

/**
 * Get database status
 */
export async function getStatus(): Promise<{
	connected: boolean;
	path?: string;
	isDemo?: boolean;
	nodeCount?: number;
	edgeCount?: number;
}> {
	if (!currentDb) {
		return { connected: false };
	}

	try {
		const stats = await currentDb.db.stats();
		// Calculate node count from snapshot + delta
		const nodeCount =
			Number(stats.snapshotNodes) +
			stats.deltaNodesCreated -
			stats.deltaNodesDeleted;
		// Calculate edge count from snapshot + delta
		const edgeCount =
			Number(stats.snapshotEdges) +
			stats.deltaEdgesAdded -
			stats.deltaEdgesDeleted;
		return {
			connected: true,
			path: currentDb.path,
			isDemo: currentDb.isDemo,
			nodeCount,
			edgeCount,
		};
	} catch {
		return { connected: false };
	}
}

/**
 * Demo Data Generator
 *
 * Creates a realistic code graph for demonstration purposes.
 * Simulates a small web server project with files, functions, classes, and their relationships.
 */

import type { Ray } from "../../../src/index.ts";
import {
  FileNode,
  FunctionNode,
  ClassNode,
  ModuleNode,
  ImportsEdge,
  CallsEdge,
  ContainsEdge,
  ExtendsEdge,
} from "./db.ts";

/**
 * Create demo graph data
 * ~30 nodes: files, functions, classes
 * ~50 edges: imports, calls, contains
 */
export async function createDemoGraph(db: Ray): Promise<void> {
  // ==========================================================================
  // Files
  // ==========================================================================
  const files = [
    { key: "src/index.ts", path: "src/index.ts", language: "typescript" },
    { key: "src/server.ts", path: "src/server.ts", language: "typescript" },
    { key: "src/router.ts", path: "src/router.ts", language: "typescript" },
    { key: "src/handlers/user.ts", path: "src/handlers/user.ts", language: "typescript" },
    { key: "src/handlers/auth.ts", path: "src/handlers/auth.ts", language: "typescript" },
    { key: "src/models/user.ts", path: "src/models/user.ts", language: "typescript" },
    { key: "src/models/session.ts", path: "src/models/session.ts", language: "typescript" },
    { key: "src/db/connection.ts", path: "src/db/connection.ts", language: "typescript" },
    { key: "src/db/queries.ts", path: "src/db/queries.ts", language: "typescript" },
    { key: "src/utils/logger.ts", path: "src/utils/logger.ts", language: "typescript" },
    { key: "src/utils/config.ts", path: "src/utils/config.ts", language: "typescript" },
    { key: "src/middleware/auth.ts", path: "src/middleware/auth.ts", language: "typescript" },
  ];

  const fileRefs = new Map<string, Awaited<ReturnType<typeof db.get>>>();
  for (const file of files) {
    const ref = await db.insert(FileNode).values(file).returning();
    fileRefs.set(file.key, ref);
  }

  // ==========================================================================
  // Classes
  // ==========================================================================
  const classes = [
    { key: "Server", name: "Server", file: "src/server.ts" },
    { key: "Router", name: "Router", file: "src/router.ts" },
    { key: "User", name: "User", file: "src/models/user.ts" },
    { key: "Session", name: "Session", file: "src/models/session.ts" },
    { key: "Database", name: "Database", file: "src/db/connection.ts" },
    { key: "Logger", name: "Logger", file: "src/utils/logger.ts" },
    { key: "BaseHandler", name: "BaseHandler", file: "src/handlers/user.ts" },
    { key: "UserHandler", name: "UserHandler", file: "src/handlers/user.ts" },
    { key: "AuthHandler", name: "AuthHandler", file: "src/handlers/auth.ts" },
  ];

  const classRefs = new Map<string, Awaited<ReturnType<typeof db.get>>>();
  for (const cls of classes) {
    const ref = await db.insert(ClassNode).values(cls).returning();
    classRefs.set(cls.key, ref);
  }

  // ==========================================================================
  // Functions
  // ==========================================================================
  const functions = [
    { key: "main", name: "main", file: "src/index.ts", line: 1n },
    { key: "startServer", name: "startServer", file: "src/server.ts", line: 10n },
    { key: "handleRequest", name: "handleRequest", file: "src/server.ts", line: 25n },
    { key: "addRoute", name: "addRoute", file: "src/router.ts", line: 5n },
    { key: "matchRoute", name: "matchRoute", file: "src/router.ts", line: 20n },
    { key: "getUsers", name: "getUsers", file: "src/handlers/user.ts", line: 10n },
    { key: "createUser", name: "createUser", file: "src/handlers/user.ts", line: 30n },
    { key: "deleteUser", name: "deleteUser", file: "src/handlers/user.ts", line: 50n },
    { key: "login", name: "login", file: "src/handlers/auth.ts", line: 10n },
    { key: "logout", name: "logout", file: "src/handlers/auth.ts", line: 40n },
    { key: "validateToken", name: "validateToken", file: "src/middleware/auth.ts", line: 5n },
    { key: "connect", name: "connect", file: "src/db/connection.ts", line: 15n },
    { key: "query", name: "query", file: "src/db/queries.ts", line: 10n },
    { key: "findUser", name: "findUser", file: "src/db/queries.ts", line: 25n },
    { key: "saveUser", name: "saveUser", file: "src/db/queries.ts", line: 40n },
    { key: "log", name: "log", file: "src/utils/logger.ts", line: 10n },
    { key: "loadConfig", name: "loadConfig", file: "src/utils/config.ts", line: 5n },
  ];

  const fnRefs = new Map<string, Awaited<ReturnType<typeof db.get>>>();
  for (const fn of functions) {
    const ref = await db.insert(FunctionNode).values(fn).returning();
    fnRefs.set(fn.key, ref);
  }

  // ==========================================================================
  // Modules (for grouping)
  // ==========================================================================
  const modules = [
    { key: "handlers", name: "handlers" },
    { key: "models", name: "models" },
    { key: "db", name: "db" },
    { key: "utils", name: "utils" },
  ];

  const moduleRefs = new Map<string, Awaited<ReturnType<typeof db.get>>>();
  for (const mod of modules) {
    const ref = await db.insert(ModuleNode).values(mod).returning();
    moduleRefs.set(mod.key, ref);
  }

  // ==========================================================================
  // Edges: Imports (file -> file)
  // ==========================================================================
  const imports = [
    ["src/index.ts", "src/server.ts"],
    ["src/index.ts", "src/utils/config.ts"],
    ["src/server.ts", "src/router.ts"],
    ["src/server.ts", "src/utils/logger.ts"],
    ["src/server.ts", "src/middleware/auth.ts"],
    ["src/router.ts", "src/handlers/user.ts"],
    ["src/router.ts", "src/handlers/auth.ts"],
    ["src/handlers/user.ts", "src/models/user.ts"],
    ["src/handlers/user.ts", "src/db/queries.ts"],
    ["src/handlers/user.ts", "src/utils/logger.ts"],
    ["src/handlers/auth.ts", "src/models/session.ts"],
    ["src/handlers/auth.ts", "src/db/queries.ts"],
    ["src/handlers/auth.ts", "src/utils/logger.ts"],
    ["src/middleware/auth.ts", "src/models/session.ts"],
    ["src/middleware/auth.ts", "src/db/queries.ts"],
    ["src/db/queries.ts", "src/db/connection.ts"],
    ["src/db/connection.ts", "src/utils/config.ts"],
  ];

  for (const [src, dst] of imports) {
    const srcRef = fileRefs.get(src);
    const dstRef = fileRefs.get(dst);
    if (srcRef && dstRef) {
      await db.link(srcRef, ImportsEdge, dstRef);
    }
  }

  // ==========================================================================
  // Edges: Contains (file -> function/class)
  // ==========================================================================
  const contains: [string, string, "function" | "class"][] = [
    ["src/index.ts", "main", "function"],
    ["src/server.ts", "Server", "class"],
    ["src/server.ts", "startServer", "function"],
    ["src/server.ts", "handleRequest", "function"],
    ["src/router.ts", "Router", "class"],
    ["src/router.ts", "addRoute", "function"],
    ["src/router.ts", "matchRoute", "function"],
    ["src/handlers/user.ts", "BaseHandler", "class"],
    ["src/handlers/user.ts", "UserHandler", "class"],
    ["src/handlers/user.ts", "getUsers", "function"],
    ["src/handlers/user.ts", "createUser", "function"],
    ["src/handlers/user.ts", "deleteUser", "function"],
    ["src/handlers/auth.ts", "AuthHandler", "class"],
    ["src/handlers/auth.ts", "login", "function"],
    ["src/handlers/auth.ts", "logout", "function"],
    ["src/models/user.ts", "User", "class"],
    ["src/models/session.ts", "Session", "class"],
    ["src/db/connection.ts", "Database", "class"],
    ["src/db/connection.ts", "connect", "function"],
    ["src/db/queries.ts", "query", "function"],
    ["src/db/queries.ts", "findUser", "function"],
    ["src/db/queries.ts", "saveUser", "function"],
    ["src/utils/logger.ts", "Logger", "class"],
    ["src/utils/logger.ts", "log", "function"],
    ["src/utils/config.ts", "loadConfig", "function"],
    ["src/middleware/auth.ts", "validateToken", "function"],
  ];

  for (const [file, entity, type] of contains) {
    const fileRef = fileRefs.get(file);
    const entityRef = type === "function" ? fnRefs.get(entity) : classRefs.get(entity);
    if (fileRef && entityRef) {
      await db.link(fileRef, ContainsEdge, entityRef);
    }
  }

  // ==========================================================================
  // Edges: Calls (function -> function)
  // ==========================================================================
  const calls = [
    ["main", "startServer"],
    ["main", "loadConfig"],
    ["startServer", "handleRequest"],
    ["startServer", "log"],
    ["handleRequest", "matchRoute"],
    ["handleRequest", "validateToken"],
    ["handleRequest", "log"],
    ["getUsers", "findUser"],
    ["getUsers", "log"],
    ["createUser", "saveUser"],
    ["createUser", "log"],
    ["deleteUser", "findUser"],
    ["deleteUser", "log"],
    ["login", "findUser"],
    ["login", "log"],
    ["logout", "log"],
    ["validateToken", "query"],
    ["findUser", "query"],
    ["saveUser", "query"],
    ["query", "connect"],
    ["connect", "loadConfig"],
  ];

  for (const [src, dst] of calls) {
    const srcRef = fnRefs.get(src);
    const dstRef = fnRefs.get(dst);
    if (srcRef && dstRef) {
      await db.link(srcRef, CallsEdge, dstRef);
    }
  }

  // ==========================================================================
  // Edges: Extends (class -> class)
  // ==========================================================================
  const extendsEdges = [
    ["UserHandler", "BaseHandler"],
    ["AuthHandler", "BaseHandler"],
  ];

  for (const [src, dst] of extendsEdges) {
    const srcRef = classRefs.get(src);
    const dstRef = classRefs.get(dst);
    if (srcRef && dstRef) {
      await db.link(srcRef, ExtendsEdge, dstRef);
    }
  }

  // ==========================================================================
  // Module contains files
  // ==========================================================================
  const moduleContains = [
    ["handlers", "src/handlers/user.ts"],
    ["handlers", "src/handlers/auth.ts"],
    ["models", "src/models/user.ts"],
    ["models", "src/models/session.ts"],
    ["db", "src/db/connection.ts"],
    ["db", "src/db/queries.ts"],
    ["utils", "src/utils/logger.ts"],
    ["utils", "src/utils/config.ts"],
  ];

  for (const [mod, file] of moduleContains) {
    const modRef = moduleRefs.get(mod);
    const fileRef = fileRefs.get(file);
    if (modRef && fileRef) {
      await db.link(modRef, ContainsEdge, fileRef);
    }
  }
}

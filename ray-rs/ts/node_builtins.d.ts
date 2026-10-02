// Minimal typings for the Node built-ins the wrapper uses. The package does not
// depend on @types/node; drop this file if it ever does.
declare module 'async_hooks' {
  export class AsyncLocalStorage<T> {
    getStore(): T | undefined
    run<R>(store: T, callback: () => R): R
  }
}

declare module 'node:crypto' {
  interface Hash {
    update(data: string): Hash
    digest(): Uint8Array
  }
  export function createHash(algorithm: string): Hash
  export function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean
}

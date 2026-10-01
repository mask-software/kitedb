// Minimal typing for the one Node built-in the wrapper uses. The package does
// not depend on @types/node; drop this file if it ever does.
declare module 'async_hooks' {
  export class AsyncLocalStorage<T> {
    getStore(): T | undefined
    run<R>(store: T, callback: () => R): R
  }
}

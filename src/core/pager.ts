/**
 * Page-based I/O abstraction for single-file database format
 * Provides page-level read/write, mmap support, and area management
 */

import { closeSync, openSync, readSync, writeSync, fstatSync, ftruncateSync, fsyncSync } from "node:fs";
import {
  DEFAULT_PAGE_SIZE,
  LOCK_BYTE_OFFSET,
  LOCK_BYTE_RANGE,
  OS_PAGE_SIZE,
} from "../constants.ts";
import type { Pager } from "../types.ts";

/**
 * FilePager implementation for single-file database
 */
export class FilePager implements Pager {
  readonly fd: number;
  readonly pageSize: number;
  readonly filePath: string;
  private _fileSize: number;
  private _mmaps: Map<string, Uint8Array> = new Map();
  private _freePages: Set<number> = new Set();

  constructor(
    fd: number,
    filePath: string,
    pageSize: number = DEFAULT_PAGE_SIZE,
    fileSize?: number,
  ) {
    this.fd = fd;
    this.filePath = filePath;
    this.pageSize = pageSize;
    
    // Get initial file size
    if (fileSize !== undefined) {
      this._fileSize = fileSize;
    } else {
      const stat = fstatSync(fd);
      this._fileSize = stat.size;
    }
  }

  get fileSize(): number {
    return this._fileSize;
  }

  /**
   * Calculate the page number for the lock byte range
   * We need to avoid allocating pages that overlap with the lock byte region
   */
  private getLockBytePageRange(): { start: number; end: number } {
    const start = Math.floor(LOCK_BYTE_OFFSET / this.pageSize);
    const end = Math.ceil((LOCK_BYTE_OFFSET + LOCK_BYTE_RANGE) / this.pageSize);
    return { start, end };
  }

  /**
   * Check if a page number overlaps with the lock byte range
   */
  private isLockBytePage(pageNum: number): boolean {
    const { start, end } = this.getLockBytePageRange();
    return pageNum >= start && pageNum < end;
  }

  /**
   * Read a single page by page number
   */
  readPage(pageNum: number): Uint8Array {
    const offset = pageNum * this.pageSize;
    
    // Safety check: don't read beyond file size
    if (offset >= this._fileSize) {
      return new Uint8Array(this.pageSize);
    }
    
    const buffer = new Uint8Array(this.pageSize);
    const bytesRead = readSync(this.fd, buffer, 0, this.pageSize, offset);
    
    // If we read less than a full page, the rest is zeros (already initialized)
    return buffer;
  }

  /**
   * Write a single page by page number
   */
  writePage(pageNum: number, data: Uint8Array): void {
    if (data.length !== this.pageSize) {
      throw new Error(`Page data must be exactly ${this.pageSize} bytes, got ${data.length}`);
    }

    // Safety check: don't write to lock byte range
    if (this.isLockBytePage(pageNum)) {
      throw new Error(`Cannot write to lock byte page range (page ${pageNum})`);
    }

    const offset = pageNum * this.pageSize;
    
    // Extend file if necessary
    const requiredSize = offset + this.pageSize;
    if (requiredSize > this._fileSize) {
      ftruncateSync(this.fd, requiredSize);
      this._fileSize = requiredSize;
    }
    
    writeSync(this.fd, data, 0, this.pageSize, offset);
    
    // Invalidate any mmap cache entries that overlap with this page
    // This ensures subsequent mmapRange calls get fresh data
    this.invalidateMmapCacheForPage(pageNum);
  }
  
  /**
   * Invalidate mmap cache entries that overlap with a specific page
   */
  private invalidateMmapCacheForPage(pageNum: number): void {
    for (const [key, _] of this._mmaps) {
      const [startStr, countStr] = key.split(":");
      const start = parseInt(startStr!, 10);
      const count = parseInt(countStr!, 10);
      if (pageNum >= start && pageNum < start + count) {
        this._mmaps.delete(key);
      }
    }
  }

  /**
   * Memory-map a range of pages (for snapshot access)
   * Returns a view into mmap'd memory using Bun.mmap for zero-copy access
   */
  mmapRange(startPage: number, pageCount: number): Uint8Array {
    const startOffset = startPage * this.pageSize;
    const length = pageCount * this.pageSize;
    
    // Validate mmap alignment
    if (startOffset % OS_PAGE_SIZE !== 0) {
      throw new Error(
        `mmap offset ${startOffset} must be aligned to OS page size ${OS_PAGE_SIZE}`
      );
    }
    
    // Check cache first
    const cacheKey = `${startPage}:${pageCount}`;
    const cached = this._mmaps.get(cacheKey);
    if (cached) {
      return cached;
    }
    
    // Use Bun.mmap for zero-copy memory mapping
    // mmap the entire file and take a subarray view for the requested range
    const fullMmap = Bun.mmap(this.filePath);
    const slice = fullMmap.subarray(startOffset, startOffset + length);
    
    this._mmaps.set(cacheKey, slice);
    return slice;
  }

  /**
   * Allocate new pages at end of file
   * Returns the starting page number of the allocated range
   */
  allocatePages(count: number): number {
    if (count <= 0) {
      throw new Error("Must allocate at least 1 page");
    }

    // Calculate current page count
    const currentPageCount = Math.ceil(this._fileSize / this.pageSize);
    let startPage = currentPageCount;
    
    // Check if we need to skip the lock byte range
    const { start: lockStart, end: lockEnd } = this.getLockBytePageRange();
    
    // If the new allocation would overlap with lock byte range, skip past it
    if (startPage < lockEnd && startPage + count > lockStart) {
      // Move start past the lock byte range
      startPage = lockEnd;
    }
    
    // Extend file
    const newSize = (startPage + count) * this.pageSize;
    ftruncateSync(this.fd, newSize);
    this._fileSize = newSize;
    
    // Invalidate mmap cache since file size changed
    // Existing mmaps may now be referencing potentially invalid regions
    this.invalidateMmapCache();
    
    return startPage;
  }

  /**
   * Mark pages as free (for vacuum)
   * In v1, this just tracks free pages; actual reclamation happens during vacuum
   */
  freePages(startPage: number, count: number): void {
    for (let i = 0; i < count; i++) {
      this._freePages.add(startPage + i);
    }
  }

  /**
   * Get count of free pages
   */
  getFreePageCount(): number {
    return this._freePages.size;
  }

  /**
   * Sync file to disk
   */
  async sync(): Promise<void> {
    fsyncSync(this.fd);
  }

  /**
   * Sync file to disk (synchronous version)
   */
  syncSync(): void {
    fsyncSync(this.fd);
  }

  /**
   * Relocate an area to a new location (for growth/compaction)
   * This is an expensive operation that copies data page by page
   */
  async relocateArea(srcPage: number, pageCount: number, dstPage: number): Promise<void> {
    if (srcPage === dstPage) {
      return;
    }

    // Validate destination doesn't overlap with lock byte range
    const { start: lockStart, end: lockEnd } = this.getLockBytePageRange();
    if (dstPage < lockEnd && dstPage + pageCount > lockStart) {
      throw new Error("Cannot relocate to lock byte range");
    }

    // Determine copy direction to avoid overwriting source before reading
    const copyForward = srcPage < dstPage;
    const buffer = new Uint8Array(this.pageSize);
    
    // Invalidate all cached mmaps upfront since this operation modifies multiple pages
    // This ensures any subsequent mmap access gets fresh data
    this.invalidateMmapCache();

    if (copyForward) {
      // Copy from end to start to avoid overwriting
      for (let i = pageCount - 1; i >= 0; i--) {
        const srcOffset = (srcPage + i) * this.pageSize;
        const dstOffset = (dstPage + i) * this.pageSize;
        
        readSync(this.fd, buffer, 0, this.pageSize, srcOffset);
        
        // Extend file if needed
        const requiredSize = dstOffset + this.pageSize;
        if (requiredSize > this._fileSize) {
          ftruncateSync(this.fd, requiredSize);
          this._fileSize = requiredSize;
        }
        
        writeSync(this.fd, buffer, 0, this.pageSize, dstOffset);
      }
    } else {
      // Copy from start to end
      for (let i = 0; i < pageCount; i++) {
        const srcOffset = (srcPage + i) * this.pageSize;
        const dstOffset = (dstPage + i) * this.pageSize;
        
        readSync(this.fd, buffer, 0, this.pageSize, srcOffset);
        
        const requiredSize = dstOffset + this.pageSize;
        if (requiredSize > this._fileSize) {
          ftruncateSync(this.fd, requiredSize);
          this._fileSize = requiredSize;
        }
        
        writeSync(this.fd, buffer, 0, this.pageSize, dstOffset);
      }
    }

    // Sync to ensure data is durable before marking old pages as free
    await this.sync();
    
    // Mark old pages as free
    this.freePages(srcPage, pageCount);
  }

  /**
   * Invalidate mmap cache
   */
  private invalidateMmapCache(): void {
    this._mmaps.clear();
  }

  /**
   * Close the pager
   */
  close(): void {
    this._mmaps.clear();
    this._freePages.clear();
    closeSync(this.fd);
  }
}

/**
 * Open a pager for an existing file
 */
export function openPager(filePath: string, pageSize: number = DEFAULT_PAGE_SIZE): FilePager {
  const fd = openSync(filePath, "r+");
  return new FilePager(fd, filePath, pageSize);
}

/**
 * Create a new pager for a new file
 */
export function createPager(filePath: string, pageSize: number = DEFAULT_PAGE_SIZE): FilePager {
  const fd = openSync(filePath, "w+");
  return new FilePager(fd, filePath, pageSize, 0);
}

/**
 * Validate that a page size is valid (power of 2, within bounds)
 */
export function isValidPageSize(pageSize: number): boolean {
  if (pageSize < 4096 || pageSize > 65536) {
    return false;
  }
  // Check power of 2
  return (pageSize & (pageSize - 1)) === 0;
}

/**
 * Calculate the number of pages needed to store a given byte count
 */
export function pagesToStore(byteCount: number, pageSize: number): number {
  return Math.ceil(byteCount / pageSize);
}

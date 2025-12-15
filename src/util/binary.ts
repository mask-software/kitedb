/**
 * Binary read/write helpers for structured I/O
 * All operations are little-endian as per spec
 */

import { SECTION_ALIGNMENT, WAL_RECORD_ALIGNMENT } from "../constants.ts";

// ============================================================================
// Buffer allocation
// ============================================================================

/**
 * Allocate a new buffer with optional initial size
 */
export function allocBuffer(size: number): Uint8Array {
  return new Uint8Array(size);
}

/**
 * Create a DataView over a Uint8Array
 */
export function viewOf(
  buffer: Uint8Array,
  offset = 0,
  length?: number,
): DataView {
  return new DataView(
    buffer.buffer,
    buffer.byteOffset + offset,
    length ?? buffer.byteLength - offset,
  );
}

// ============================================================================
// Alignment utilities
// ============================================================================

/**
 * Round up to alignment boundary
 */
export function alignUp(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

/**
 * Calculate padding needed to reach alignment
 */
export function paddingFor(value: number, alignment: number): number {
  const remainder = value % alignment;
  return remainder === 0 ? 0 : alignment - remainder;
}

/**
 * Round up to section alignment (64 bytes)
 */
export function alignSection(offset: number): number {
  return alignUp(offset, SECTION_ALIGNMENT);
}

/**
 * Round up to WAL record alignment (8 bytes)
 */
export function alignWalRecord(offset: number): number {
  return alignUp(offset, WAL_RECORD_ALIGNMENT);
}

// ============================================================================
// Read helpers (all little-endian)
// ============================================================================

export function readU8(view: DataView, offset: number): number {
  return view.getUint8(offset);
}

export function readU16(view: DataView, offset: number): number {
  return view.getUint16(offset, true);
}

export function readU32(view: DataView, offset: number): number {
  return view.getUint32(offset, true);
}

export function readI32(view: DataView, offset: number): number {
  return view.getInt32(offset, true);
}

export function readU64(view: DataView, offset: number): bigint {
  return view.getBigUint64(offset, true);
}

export function readI64(view: DataView, offset: number): bigint {
  return view.getBigInt64(offset, true);
}

export function readF64(view: DataView, offset: number): number {
  return view.getFloat64(offset, true);
}

// ============================================================================
// Write helpers (all little-endian)
// ============================================================================

export function writeU8(view: DataView, offset: number, value: number): void {
  view.setUint8(offset, value);
}

export function writeU16(view: DataView, offset: number, value: number): void {
  view.setUint16(offset, value, true);
}

export function writeU32(view: DataView, offset: number, value: number): void {
  view.setUint32(offset, value, true);
}

export function writeI32(view: DataView, offset: number, value: number): void {
  view.setInt32(offset, value, true);
}

export function writeU64(view: DataView, offset: number, value: bigint): void {
  view.setBigUint64(offset, value, true);
}

export function writeI64(view: DataView, offset: number, value: bigint): void {
  view.setBigInt64(offset, value, true);
}

export function writeF64(view: DataView, offset: number, value: number): void {
  view.setFloat64(offset, value, true);
}

// ============================================================================
// Array read helpers
// ============================================================================

/**
 * Read u32 at array index
 */
export function readU32At(view: DataView, index: number): number {
  return view.getUint32(index * 4, true);
}

/**
 * Read i32 at array index
 */
export function readI32At(view: DataView, index: number): number {
  return view.getInt32(index * 4, true);
}

/**
 * Read u64 at array index
 */
export function readU64At(view: DataView, index: number): bigint {
  return view.getBigUint64(index * 8, true);
}

// ============================================================================
// Array write helpers
// ============================================================================

/**
 * Write u32 at array index
 */
export function writeU32At(view: DataView, index: number, value: number): void {
  view.setUint32(index * 4, value, true);
}

/**
 * Write i32 at array index
 */
export function writeI32At(view: DataView, index: number, value: number): void {
  view.setInt32(index * 4, value, true);
}

/**
 * Write u64 at array index
 */
export function writeU64At(view: DataView, index: number, value: bigint): void {
  view.setBigUint64(index * 8, value, true);
}

// ============================================================================
// String encoding
// ============================================================================

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder("utf-8");

/**
 * Encode string to UTF-8 bytes
 */
export function encodeString(str: string): Uint8Array {
  return textEncoder.encode(str);
}

/**
 * Decode UTF-8 bytes to string
 */
export function decodeString(bytes: Uint8Array): string {
  return textDecoder.decode(bytes);
}

/**
 * Read a string from buffer given offset and length
 */
export function readString(
  buffer: Uint8Array,
  offset: number,
  length: number,
): string {
  return decodeString(buffer.subarray(offset, offset + length));
}

// ============================================================================
// Buffer building utilities
// ============================================================================

/**
 * Dynamic buffer builder for constructing binary data
 */
export class BufferBuilder {
  private chunks: Uint8Array[] = [];
  private currentChunk: Uint8Array;
  private currentView: DataView;
  private position = 0;
  private totalSize = 0;
  private readonly chunkSize: number;

  constructor(initialSize = 4096) {
    this.chunkSize = initialSize;
    this.currentChunk = new Uint8Array(initialSize);
    this.currentView = new DataView(this.currentChunk.buffer);
  }

  private ensureCapacity(needed: number): void {
    if (this.position + needed <= this.currentChunk.length) return;

    // Save current chunk (only used portion)
    if (this.position > 0) {
      this.chunks.push(this.currentChunk.subarray(0, this.position));
      this.totalSize += this.position;
    }

    // Allocate new chunk
    const size = Math.max(this.chunkSize, needed);
    this.currentChunk = new Uint8Array(size);
    this.currentView = new DataView(this.currentChunk.buffer);
    this.position = 0;
  }

  get offset(): number {
    return this.totalSize + this.position;
  }

  writeU8(value: number): this {
    this.ensureCapacity(1);
    this.currentView.setUint8(this.position++, value);
    return this;
  }

  writeU16(value: number): this {
    this.ensureCapacity(2);
    this.currentView.setUint16(this.position, value, true);
    this.position += 2;
    return this;
  }

  writeU32(value: number): this {
    this.ensureCapacity(4);
    this.currentView.setUint32(this.position, value, true);
    this.position += 4;
    return this;
  }

  writeI32(value: number): this {
    this.ensureCapacity(4);
    this.currentView.setInt32(this.position, value, true);
    this.position += 4;
    return this;
  }

  writeU64(value: bigint): this {
    this.ensureCapacity(8);
    this.currentView.setBigUint64(this.position, value, true);
    this.position += 8;
    return this;
  }

  writeI64(value: bigint): this {
    this.ensureCapacity(8);
    this.currentView.setBigInt64(this.position, value, true);
    this.position += 8;
    return this;
  }

  writeF64(value: number): this {
    this.ensureCapacity(8);
    this.currentView.setFloat64(this.position, value, true);
    this.position += 8;
    return this;
  }

  writeBytes(data: Uint8Array): this {
    this.ensureCapacity(data.length);
    this.currentChunk.set(data, this.position);
    this.position += data.length;
    return this;
  }

  writeZeros(count: number): this {
    this.ensureCapacity(count);
    // Uint8Array is zero-initialized, but be explicit
    this.currentChunk.fill(0, this.position, this.position + count);
    this.position += count;
    return this;
  }

  /**
   * Pad to alignment boundary with zeros
   */
  alignTo(alignment: number): this {
    const padding = paddingFor(this.offset, alignment);
    if (padding > 0) {
      this.writeZeros(padding);
    }
    return this;
  }

  /**
   * Build final buffer
   */
  build(): Uint8Array {
    // Add current chunk
    if (this.position > 0) {
      this.chunks.push(this.currentChunk.subarray(0, this.position));
      this.totalSize += this.position;
    }

    // Concatenate all chunks
    const result = new Uint8Array(this.totalSize);
    let offset = 0;
    for (const chunk of this.chunks) {
      result.set(chunk, offset);
      offset += chunk.length;
    }

    return result;
  }
}

// ============================================================================
// Bitwise utilities for property encoding
// ============================================================================

/**
 * Reinterpret f64 as u64 bits
 */
export function f64ToU64Bits(value: number): bigint {
  const buffer = new ArrayBuffer(8);
  const view = new DataView(buffer);
  view.setFloat64(0, value, true);
  return view.getBigUint64(0, true);
}

/**
 * Reinterpret u64 bits as f64
 */
export function u64BitsToF64(bits: bigint): number {
  const buffer = new ArrayBuffer(8);
  const view = new DataView(buffer);
  view.setBigUint64(0, bits, true);
  return view.getFloat64(0, true);
}

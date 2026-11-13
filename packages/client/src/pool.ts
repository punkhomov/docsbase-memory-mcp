import { performance } from 'node:perf_hooks';
import { Connection } from './connection.ts';
import { DocsbaseError, abortError } from './types.ts';

interface Slot {
  connection: Connection | undefined;
  busy: boolean;
}
export interface Lease {
  readonly slot: Slot;
  readonly queueMs: number;
}
interface Waiter {
  readonly queuedAt: number;
  readonly signal: AbortSignal;
  readonly resolve: (lease: Lease) => void;
  readonly reject: (error: DocsbaseError) => void;
  readonly cleanup: () => void;
}
/** Policy-free bounded FIFO pool. Slots are reserved before any async connect. */
export class Pool {
  private readonly capacity: number;
  private readonly queueLimit: number;
  private readonly slots = new Set<Slot>();
  private readonly waiting: Waiter[] = [];
  private closed = false;
  constructor(capacity: number, queueLimit: number) {
    this.capacity = capacity;
    this.queueLimit = queueLimit;
  }
  async acquire(signal: AbortSignal): Promise<Lease> {
    if (signal.aborted) throw abortError(signal);
    if (this.closed) throw new DocsbaseError('closed', 'docsbase client closed');
    // Dead idle sockets consume no capacity on subsequent admission.
    for (const slot of this.slots) {
      if (!slot.busy && !slot.connection?.alive) {
        slot.connection?.close();
        this.slots.delete(slot);
      }
    }
    this.drain();
    const slot = this.reserve();
    if (slot) return { slot, queueMs: 0 };
    if (this.waiting.length >= this.queueLimit)
      throw new DocsbaseError('overloaded', 'docsbase client queue full');
    return new Promise<Lease>((resolve, reject) => {
      const aborted = () => {
        const index = this.waiting.indexOf(waiter);
        if (index >= 0) {
          this.waiting.splice(index, 1);
          waiter.cleanup();
          reject(abortError(signal));
        }
      };
      const waiter: Waiter = {
        queuedAt: performance.now(),
        signal,
        resolve,
        reject,
        cleanup: () => signal.removeEventListener('abort', aborted),
      };
      this.waiting.push(waiter);
      signal.addEventListener('abort', aborted, { once: true });
      if (signal.aborted) aborted();
    });
  }
  release(lease: Lease): void {
    const slot = lease.slot;
    if (!this.slots.has(slot)) return;
    slot.busy = false;
    if (this.closed || !slot.connection?.alive) {
      slot.connection?.close();
      this.slots.delete(slot);
    }
    this.drain();
  }
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const waiter of this.waiting.splice(0)) {
      waiter.cleanup();
      waiter.reject(new DocsbaseError('closed', 'docsbase client closed'));
    }
    for (const slot of this.slots) slot.connection?.close();
    this.slots.clear();
  }
  private reserve(): Slot | undefined {
    for (const slot of this.slots) {
      if (!slot.busy) {
        slot.busy = true;
        return slot;
      }
    }
    if (this.slots.size >= this.capacity) return undefined;
    const slot: Slot = { connection: undefined, busy: true };
    this.slots.add(slot);
    return slot;
  }
  private drain(): void {
    while (!this.closed && this.waiting.length) {
      const waiter = this.waiting[0]!;
      if (waiter.signal.aborted) {
        this.waiting.shift();
        waiter.cleanup();
        waiter.reject(abortError(waiter.signal));
        continue;
      }
      const slot = this.reserve();
      if (!slot) break;
      this.waiting.shift();
      waiter.cleanup();
      waiter.resolve({
        slot,
        queueMs: Math.floor(Math.max(0, performance.now() - waiter.queuedAt)),
      });
    }
  }
}

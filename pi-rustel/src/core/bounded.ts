/**
 * A bounded FIFO log: the newest `limit` entries, the oldest dropped.
 *
 * Every buffer the extension keeps over a long-running rustel child is
 * bounded, so a set that narrates for hours cannot grow memory without end.
 */
export class BoundedLog<T> {
  private readonly entries: T[] = [];
  private readonly limit: number;

  constructor(limit: number) {
    if (limit < 1) {
      throw new Error(`a bounded log needs a limit of at least 1, got ${limit}`);
    }
    this.limit = limit;
  }

  /** Append an entry, dropping the oldest when the log is full. */
  push(entry: T): void {
    this.entries.push(entry);
    if (this.entries.length > this.limit) {
      this.entries.shift();
    }
  }

  get length(): number {
    return this.entries.length;
  }

  /** Drop every held entry. */
  clear(): void {
    this.entries.length = 0;
  }

  /** A copy of the held entries, oldest first. */
  snapshot(): readonly T[] {
    return [...this.entries];
  }
}

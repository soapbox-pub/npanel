/** How many tokens a key gets, and how long a spent bucket takes to refill. */
export interface RateLimit {
  /** Tokens a key starts with, and the most it can bank. */
  capacity: number;
  /** Milliseconds in which an empty bucket refills completely. */
  interval: number;
}

interface Bucket {
  tokens: number;
  /** When `tokens` was last recomputed. */
  updated: number;
}

/**
 * A token bucket per key, with a bounded number of keys.
 *
 * The limit is passed to {@link RateLimiter.take} rather than held here so
 * that a setting an admin changes on the dashboard applies to the next
 * request, with nothing to resubscribe.
 *
 * Keys can come from outside — a hostname someone else chose — so the map is
 * capped. Buckets that have refilled to capacity are indistinguishable from
 * ones that never existed and are dropped first; past that, the oldest go. An
 * attacker who floods keys to evict someone else's bucket has to spend the
 * caller's budget doing it, which is what a second, global limiter is for.
 */
export class RateLimiter {
  private readonly buckets = new Map<string, Bucket>();
  private readonly maxKeys: number;
  private readonly now: () => number;

  constructor(opts: { maxKeys?: number; now?: () => number } = {}) {
    this.maxKeys = opts.maxKeys ?? 4096;
    this.now = opts.now ?? (() => Date.now());
  }

  /** Spend one token for `key`, or return false when the bucket is empty. */
  take(key: string, limit: RateLimit): boolean {
    const now = this.now();
    const bucket = this.buckets.get(key);

    if (!bucket) {
      this.evict(limit, now);
      this.buckets.set(key, { tokens: limit.capacity - 1, updated: now });
      return true;
    }

    const tokens = refill(bucket, limit, now);
    if (tokens < 1) {
      // Store the partial token so a denied call still banks what it accrued.
      bucket.tokens = tokens;
      bucket.updated = now;
      return false;
    }

    bucket.tokens = tokens - 1;
    bucket.updated = now;
    return true;
  }

  /** Number of keys currently tracked. */
  get size(): number {
    return this.buckets.size;
  }

  private evict(limit: RateLimit, now: number): void {
    if (this.buckets.size < this.maxKeys) return;

    for (const [key, bucket] of this.buckets) {
      if (refill(bucket, limit, now) >= limit.capacity) this.buckets.delete(key);
    }

    // Map iteration is insertion-ordered, so this drops the keys first seen.
    for (const key of this.buckets.keys()) {
      if (this.buckets.size < this.maxKeys) break;
      this.buckets.delete(key);
    }
  }
}

/** A bucket's token count brought up to date, capped at capacity. */
function refill(bucket: Bucket, limit: RateLimit, now: number): number {
  const elapsed = Math.max(0, now - bucket.updated);
  return Math.min(limit.capacity, bucket.tokens + (elapsed / limit.interval) * limit.capacity);
}

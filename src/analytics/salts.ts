import { createHmac } from "node:crypto";
import type { AnalyticsStore } from "./store.ts";

const DAY = 24 * 60 * 60;

/**
 * The daily salts visitor ids are hashed with, the way Plausible does it.
 *
 * A visitor is never stored as themselves. What is stored is a hash of their
 * user agent, address and the site, under a salt that is replaced every day at
 * midnight UTC and destroyed a day after that. The same person is the same id
 * for one day on one site, and nothing links them further than that — not
 * across sites, since the site is hashed in, and not across days, since
 * yesterday's salt no longer exists.
 *
 * The previous salt is kept for one day for the sake of visits in progress: a
 * visitor browsing across midnight hashes differently afterwards, and their
 * session is found by trying the old salt as well.
 */
export class Salts {
  private current!: Uint8Array;
  private previous: Uint8Array | undefined;
  /** Epoch seconds of the midnight the current salt belongs to. */
  private day = 0;

  constructor(private readonly store: AnalyticsStore) {}

  /** Rotate if the current salt is from before today. Cheap enough to call per event. */
  refresh(now: number): void {
    const today = now - (now % DAY);
    if (this.day === today) return;

    const stored = this.store.salts();
    const newest = stored[0];

    if (newest && newest.createdAt >= today) {
      this.current = newest.salt;
      this.previous = stored[1]?.salt;
    } else {
      this.previous = newest?.salt;
      this.current = this.store.rotateSalt(now);
    }

    this.day = today;
  }

  /**
   * A visitor's id under the current salt, and under the previous one if there
   * is one.
   *
   * 48 bits of an HMAC: exactly representable as a JavaScript number, and
   * enough that two visitors to one site on one day colliding is a
   * one-in-millions event even for a busy site.
   */
  userIds(now: number, parts: string[]): { current: number; previous: number | undefined } {
    this.refresh(now);
    const input = parts.join("");

    return {
      current: hash(this.current, input),
      previous: this.previous ? hash(this.previous, input) : undefined,
    };
  }
}

function hash(salt: Uint8Array, input: string): number {
  return createHmac("sha256", salt).update(input).digest().readUIntBE(0, 6);
}

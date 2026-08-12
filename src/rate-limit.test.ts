import assert from "node:assert/strict";
import { test } from "node:test";
import { RateLimiter } from "./rate-limit.ts";

const MINUTE = 60 * 1000;

/** A limiter whose clock the test moves by hand. */
function fixedClock(opts: { maxKeys?: number } = {}) {
  let now = 0;
  const limiter = new RateLimiter({ ...opts, now: () => now });

  return {
    limiter,
    advance(ms: number) {
      now += ms;
    },
  };
}

test("spends a bucket and refuses once it is empty", () => {
  const { limiter } = fixedClock();
  const limit = { capacity: 3, interval: 10 * MINUTE };

  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), false);
});

test("keys are independent", () => {
  const { limiter } = fixedClock();
  const limit = { capacity: 1, interval: 10 * MINUTE };

  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), false);
  assert.equal(limiter.take("b.example.com", limit), true);
});

test("refills gradually, capped at capacity", () => {
  const { limiter, advance } = fixedClock();
  const limit = { capacity: 2, interval: 10 * MINUTE };

  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), false);

  // Half the interval buys back one of the two tokens, and no more.
  advance(5 * MINUTE);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), false);

  // Idling far longer than the interval does not bank extra tokens.
  advance(10 * 10 * MINUTE);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), true);
  assert.equal(limiter.take("a.example.com", limit), false);
});

test("a denied call still banks the tokens it accrued", () => {
  const { limiter, advance } = fixedClock();
  const limit = { capacity: 1, interval: 10 * MINUTE };

  assert.equal(limiter.take("a.example.com", limit), true);

  // Four denials, each a quarter of the way to a token: the fractions have to
  // survive, or a caller that polls often would never be allowed again.
  for (let i = 0; i < 4; i++) {
    advance(MINUTE);
    assert.equal(limiter.take("a.example.com", limit), false);
  }

  advance(6 * MINUTE);
  assert.equal(limiter.take("a.example.com", limit), true);
});

test("caps how many keys it tracks", () => {
  const { limiter } = fixedClock({ maxKeys: 8 });
  const limit = { capacity: 2, interval: 10 * MINUTE };

  // Keys nobody has spent are dropped first, so a flood of one-off names does
  // not grow the map without bound.
  for (let i = 0; i < 500; i++) {
    assert.equal(limiter.take(`host-${i}.example.com`, limit), true);
  }

  assert.ok(limiter.size <= 8, `tracked ${limiter.size} keys`);
});

test("evicts refilled buckets before spent ones", () => {
  const { limiter, advance } = fixedClock({ maxKeys: 4 });
  const limit = { capacity: 2, interval: 10 * MINUTE };

  for (let i = 0; i < 3; i++) limiter.take(`quiet-${i}.example.com`, limit);

  // Those three are back at capacity, so forgetting them changes nothing.
  advance(10 * MINUTE);

  assert.equal(limiter.take("busy.example.com", limit), true);
  assert.equal(limiter.take("busy.example.com", limit), true);
  assert.equal(limiter.take("busy.example.com", limit), false);

  // Crossing the cap must not hand `busy` a fresh bucket, even though it is
  // the oldest key still tracked.
  limiter.take("newcomer.example.com", limit);

  assert.equal(limiter.take("busy.example.com", limit), false);
});

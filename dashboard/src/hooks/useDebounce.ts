import { useEffect, useState } from 'react';

/**
 * A copy of `value` that only catches up once it has stopped changing.
 *
 * Typing is far faster than a relay round-trip, so anything that queries on
 * keystroke goes through here first.
 */
export function useDebounce<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(timer);
  }, [value, delay]);

  return debounced;
}

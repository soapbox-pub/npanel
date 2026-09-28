import { useCallback, useMemo } from 'react';
import { useSearchParams } from 'react-router-dom';

import { type DashboardState, parseState, serializeState } from '@/lib/analytics/state';

/** Query parameters the page keeps for itself, alongside the dashboard state. */
const PAGE_PARAMS = ['details'];

/**
 * The analytics page's state, read from and written to the URL, so every view
 * is a link and the back button undoes a filter.
 */
export function useDashboardState(): [DashboardState, (next: DashboardState) => void] {
  const [search, setSearch] = useSearchParams();
  const state = useMemo(() => parseState(search), [search]);

  const update = useCallback(
    (next: DashboardState) => {
      setSearch((current) => {
        const params = serializeState(next);
        for (const name of PAGE_PARAMS) {
          const value = current.get(name);
          if (value) params.set(name, value);
        }
        return params;
      });
    },
    [setSearch],
  );

  return [state, update];
}

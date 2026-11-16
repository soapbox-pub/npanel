import { useMemo } from 'react';

import { useDebounce } from '@/hooks/useDebounce';
import { useResolvedAddress } from '@/hooks/useNpanel';
import { type NsiteAddress, parseNsiteAddress, webPageUrl } from '@shared/nsite-address';

export interface ParsedAddress {
  address: NsiteAddress | undefined;
  error: string | undefined;
  /** The host of a web page being asked which nsite it is, while it's asked. */
  checking?: string;
}

/**
 * Parse an address the way the server will.
 *
 * The parser module is shared with the server, so the preview shown while
 * typing is exactly what will be stored. A web page off any gateway is the
 * exception: only the server can read its `Nsite-Location`, so it's asked,
 * once typing stops.
 */
export function useParsedAddress(value: string): ParsedAddress {
  const local = useMemo(() => {
    if (!value.trim()) return { address: undefined, error: undefined, page: undefined };
    try {
      return { address: parseNsiteAddress(value), error: undefined, page: undefined };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return { address: undefined, error: message, page: webPageUrl(value) };
    }
  }, [value]);

  const page = local.page?.href;
  const settled = useDebounce(page, 400);
  const lookup = useResolvedAddress(settled === page ? page : undefined);

  if (!local.page) return { address: local.address, error: local.error };
  if (settled !== page || lookup.isPending) {
    return { address: undefined, error: undefined, checking: local.page.hostname };
  }
  if (lookup.error) {
    return { address: undefined, error: lookup.error.message };
  }
  return { address: lookup.data, error: undefined };
}

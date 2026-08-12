import { useMemo } from 'react';

import { type NsiteAddress, parseNsiteAddress } from '@shared/nsite-address';

export interface ParsedAddress {
  address: NsiteAddress | undefined;
  error: string | undefined;
}

/**
 * Parse an address the way the server will.
 *
 * The parser module is shared with the server, so the preview shown while
 * typing is exactly what will be stored.
 */
export function useParsedAddress(value: string): ParsedAddress {
  return useMemo(() => {
    if (!value.trim()) return { address: undefined, error: undefined };
    try {
      return { address: parseNsiteAddress(value), error: undefined };
    } catch (error) {
      return { address: undefined, error: error instanceof Error ? error.message : String(error) };
    }
  }, [value]);
}

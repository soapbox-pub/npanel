import { useMemo } from 'react';

import { useCurrentUser } from '@/hooks/useCurrentUser';
import { ApiClient } from '@/lib/api';

/**
 * An API client bound to the signed-in account, or undefined when logged out.
 *
 * Recreated whenever the signer changes so switching accounts never reuses
 * another account's signer.
 */
export function useApi(): ApiClient | undefined {
  const { user } = useCurrentUser();

  return useMemo(() => (user ? new ApiClient(user.signer) : undefined), [user]);
}

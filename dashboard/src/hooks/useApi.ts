import { useMemo } from 'react';

import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useServer } from '@/hooks/useServer';
import { ApiClient } from '@/lib/api';

/**
 * An API client bound to the signed-in account, or undefined when logged out.
 *
 * Recreated whenever the signer or the server changes, so switching accounts
 * never reuses another account's signer.
 */
export function useApi(): ApiClient | undefined {
  const { user } = useCurrentUser();
  const { origin } = useServer();

  return useMemo(() => (user ? new ApiClient(user.signer, origin) : undefined), [user, origin]);
}

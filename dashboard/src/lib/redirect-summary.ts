import type { Redirect } from '@/lib/api-types';

/** Where a redirect sends visitors, as short as it can be said. */
export function redirectSummary(redirect: Redirect): string {
  return redirect.keep_path ? `${redirect.target.replace(/\/$/, '')}/…` : redirect.target;
}

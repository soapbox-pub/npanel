import { useState } from 'react';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Skeleton } from '@/components/ui/skeleton';
import { useGrants, useSetGrants, useUsers } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/**
 * Who may manage this hostname.
 *
 * Admins always have access; staff see only the hostnames granted to them, so
 * this list is what makes a staff account useful.
 */
export function GrantsEditor({ hostId }: { hostId: number }) {
  const users = useUsers();
  const grants = useGrants(hostId);
  const setGrants = useSetGrants();
  const { toast } = useToast();

  // Undefined until the admin edits something, so the server's answer shows
  // through without an effect copying it into state.
  const [draft, setDraft] = useState<string[] | undefined>();

  if (users.isLoading || grants.isLoading) {
    return <Skeleton className="h-32 w-full" />;
  }

  const selected = draft ?? (grants.data ?? []).map((grant) => grant.user_pubkey);
  const staff = (users.data ?? []).filter((user) => user.role === 'staff');
  const admins = (users.data ?? []).filter((user) => user.role === 'admin');

  const toggle = (pubkey: string) => {
    setDraft(
      selected.includes(pubkey) ? selected.filter((value) => value !== pubkey) : [...selected, pubkey],
    );
  };

  const save = async () => {
    try {
      await setGrants.mutateAsync({ id: hostId, pubkeys: selected });
      setDraft(undefined);
      toast({ title: 'Access updated' });
    } catch (error) {
      toast({
        title: "Couldn't update access",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-4">
      {!staff.length ? (
        <p className="text-sm text-muted-foreground">
          No staff users yet. Add one on the Users page to grant scoped access.
        </p>
      ) : (
        <ul className="space-y-2">
          {staff.map((user) => (
            <li key={user.pubkey} className="flex items-center gap-3">
              <Checkbox
                id={`grant-${user.pubkey}`}
                checked={selected.includes(user.pubkey)}
                onCheckedChange={() => toggle(user.pubkey)}
              />
              <label htmlFor={`grant-${user.pubkey}`} className="cursor-pointer">
                <PubkeyLabel pubkey={user.pubkey} />
              </label>
            </li>
          ))}
        </ul>
      )}

      {!!admins.length && (
        <p className="text-xs text-muted-foreground">
          Admins ({admins.length}) can manage every hostname and are not listed here.
        </p>
      )}

      <div className="flex items-center gap-3">
        <Button onClick={save} disabled={setGrants.isPending || !staff.length}>
          {setGrants.isPending ? 'Saving…' : 'Save access'}
        </Button>
        <Badge variant="outline">{selected.length} granted</Badge>
      </div>
    </div>
  );
}

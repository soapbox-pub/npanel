import { useState } from 'react';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Skeleton } from '@/components/ui/skeleton';
import { useUsers } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

interface AccessEditorProps {
  /** Pubkeys currently granted, or undefined while they load. */
  granted: string[] | undefined;
  loading: boolean;
  saving: boolean;
  onSave: (pubkeys: string[]) => Promise<unknown>;
  /** What admins can reach without a grant, for the note underneath. */
  scope: string;
}

/**
 * Who may manage one thing.
 *
 * Shared by sites and bunkers because the rule is the same for both: admins
 * have everything, and staff have exactly what this list says.
 */
export function AccessEditor({ granted, loading, saving, onSave, scope }: AccessEditorProps) {
  const users = useUsers();
  const { toast } = useToast();

  // Undefined until the admin edits something, so the server's answer shows
  // through without an effect copying it into state.
  const [draft, setDraft] = useState<string[] | undefined>();

  if (users.isLoading || loading) {
    return <Skeleton className="h-32 w-full" />;
  }

  const selected = draft ?? granted ?? [];
  const staff = (users.data ?? []).filter((user) => user.role === 'staff');
  const admins = (users.data ?? []).filter((user) => user.role === 'admin');

  const toggle = (pubkey: string) => {
    setDraft(
      selected.includes(pubkey) ? selected.filter((value) => value !== pubkey) : [...selected, pubkey],
    );
  };

  const save = async () => {
    try {
      await onSave(selected);
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
          Admins ({admins.length}) can manage every {scope} and are not listed here.
        </p>
      )}

      <div className="flex items-center gap-3">
        <Button onClick={save} disabled={saving || !staff.length}>
          {saving ? 'Saving…' : 'Save access'}
        </Button>
        <Badge variant="outline">{selected.length} granted</Badge>
      </div>
    </div>
  );
}

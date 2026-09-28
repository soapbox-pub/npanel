import { useSeoMeta } from '@unhead/react';
import { useState } from 'react';

import { Row, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { PubkeySearchInput } from '@/components/PubkeySearchInput';
import { Button } from '@/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { useCreateUser, useDeleteUser, useMe, useUpdateUser, useUsers } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { shorten } from '@/lib/format';

type Role = 'admin' | 'staff';

/** Who can sign in, and with what role. Admin only. */
export function UsersPage() {
  useSeoMeta({ title: 'Users — npanel' });

  const users = useUsers();
  const me = useMe();
  const createUser = useCreateUser();
  const updateUser = useUpdateUser();
  const deleteUser = useDeleteUser();
  const { toast } = useToast();

  const [pubkey, setPubkey] = useState('');
  const [role, setRole] = useState<Role>('staff');

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      toast({ title: success });
    } catch (error) {
      toast({
        title: "That didn't work",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-8">
      <PageHeader title="Users" />

      <Section title="Add a user">
        <form
          className="flex flex-wrap items-start gap-3 p-4"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              await createUser.mutateAsync({ pubkey, role });
              setPubkey('');
            }, 'User added');
          }}
        >
          <div className="min-w-56 flex-1">
            <PubkeySearchInput
              id="new-user"
              value={pubkey}
              onChange={setPubkey}
              placeholder="Name or npub"
              aria-label="Name or npub"
            />
          </div>
          <RoleSelect value={role} onChange={setRole} label="Role" />
          <Button type="submit" disabled={!pubkey.trim() || createUser.isPending}>
            Add
          </Button>
        </form>
      </Section>

      {users.isLoading ? (
        <Skeleton className="h-40 w-full rounded-xl" />
      ) : (
        <Section title="Everyone" footer="Admins manage everything. Staff see only the sites granted to them.">
          {users.data?.map((user) => (
            <Row
              key={user.pubkey}
              label={<PubkeyLabel pubkey={user.pubkey} />}
              description={
                <>
                  <span className="font-mono">{shorten(user.npub)}</span>
                  {user.role === 'staff' &&
                    ` · ${user.hostnames.length} ${user.hostnames.length === 1 ? 'site' : 'sites'}`}
                </>
              }
            >
              <div className="flex items-center gap-2">
                <RoleSelect
                  value={user.role}
                  label={`Role of ${shorten(user.npub)}`}
                  onChange={(value) =>
                    run(() => updateUser.mutateAsync({ pubkey: user.pubkey, role: value }), 'Role changed')
                  }
                />
                {user.pubkey !== me.data?.pubkey && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="text-destructive hover:bg-destructive/10"
                    onClick={() => run(() => deleteUser.mutateAsync(user.pubkey), 'User removed')}
                  >
                    Remove
                  </Button>
                )}
              </div>
            </Row>
          ))}
        </Section>
      )}
    </div>
  );
}

function RoleSelect({ value, onChange, label }: { value: Role; onChange(value: Role): void; label: string }) {
  return (
    <Select value={value} onValueChange={(next) => onChange(next as Role)}>
      <SelectTrigger className="w-28" aria-label={label}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="staff">Staff</SelectItem>
        <SelectItem value="admin">Admin</SelectItem>
      </SelectContent>
    </Select>
  );
}

export default UsersPage;

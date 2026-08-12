import { useSeoMeta } from '@unhead/react';
import { useState } from 'react';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { shorten } from '@/lib/format';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useCreateUser, useDeleteUser, useMe, useUpdateUser, useUsers } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

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
  const [role, setRole] = useState<'admin' | 'staff'>('staff');

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      toast({ title: success });
    } catch (error) {
      toast({
        title: 'That did not work',
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Users</h1>
        <p className="text-sm text-muted-foreground">
          Admins manage everything. Staff only see the hostnames granted to them.
        </p>
      </div>

      <Card>
        <CardContent className="flex flex-wrap items-end gap-3 pt-6">
          <div className="min-w-64 flex-1 space-y-2">
            <Label htmlFor="new-user">npub or hex pubkey</Label>
            <Input
              id="new-user"
              value={pubkey}
              onChange={(event) => setPubkey(event.target.value)}
              placeholder="npub1…"
              autoComplete="off"
              spellCheck={false}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="new-user-role">Role</Label>
            <Select value={role} onValueChange={(value) => setRole(value as 'admin' | 'staff')}>
              <SelectTrigger id="new-user-role" className="w-32">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="staff">Staff</SelectItem>
                <SelectItem value="admin">Admin</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <Button
            disabled={!pubkey.trim() || createUser.isPending}
            onClick={() =>
              run(async () => {
                await createUser.mutateAsync({ pubkey, role });
                setPubkey('');
              }, 'User added')
            }
          >
            Add user
          </Button>
        </CardContent>
      </Card>

      {users.isLoading ? (
        <Skeleton className="h-40 w-full" />
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>User</TableHead>
                <TableHead>npub</TableHead>
                <TableHead>Role</TableHead>
                <TableHead>Hostnames</TableHead>
                <TableHead className="text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {users.data?.map((user) => (
                <TableRow key={user.pubkey}>
                  <TableCell>
                    <PubkeyLabel pubkey={user.pubkey} />
                  </TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    {shorten(user.npub)}
                  </TableCell>
                  <TableCell>
                    <Select
                      value={user.role}
                      onValueChange={(value) =>
                        run(
                          () => updateUser.mutateAsync({ pubkey: user.pubkey, role: value as 'admin' | 'staff' }),
                          'Role updated',
                        )
                      }
                    >
                      <SelectTrigger className="w-28">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="staff">Staff</SelectItem>
                        <SelectItem value="admin">Admin</SelectItem>
                      </SelectContent>
                    </Select>
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {user.role === 'admin' ? 'all' : user.host_ids.length}
                  </TableCell>
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={user.pubkey === me.data?.pubkey}
                      onClick={() => run(() => deleteUser.mutateAsync(user.pubkey), 'User removed')}
                    >
                      Remove
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Card>
      )}
    </div>
  );
}

export default UsersPage;

import { KeyRound, Trash2 } from 'lucide-react';
import { useState } from 'react';

import { PermissionPicker } from '@/components/bunker/PermissionPicker';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useConnections, useRevokeConnection, useUpdateConnection } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { BunkerConnection } from '@/lib/api-types';
import { type PermissionDraft, draftToSpecs, permissionLabel, specsToDraft } from '@/lib/permissions';

/** Everything minted from one bunker, and who minted it. */
export function ConnectionList({ slug }: { slug: string }) {
  const connections = useConnections(slug);

  if (connections.isLoading) {
    return <Skeleton className="h-40 w-full" />;
  }

  if (connections.isError) {
    return (
      <Card className="border-t-2 border-t-destructive">
        <CardContent className="pt-6 text-sm text-destructive">
          {connections.error instanceof Error ? connections.error.message : 'Failed to load connections.'}
        </CardContent>
      </Card>
    );
  }

  const rows = connections.data ?? [];

  if (!rows.length) {
    return (
      <Card className="border-dashed">
        <CardContent className="flex flex-col items-center gap-3 py-10 text-center">
          <div className="rounded-full bg-accent p-3">
            <KeyRound className="h-5 w-5 text-muted-foreground" />
          </div>
          <p className="text-sm text-muted-foreground">
            Nothing signs through this bunker yet. Mint a connection to authorize something.
          </p>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex items-center justify-between gap-3 border-b bg-muted/50 px-6 py-2.5">
        <h2 className="font-mono text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
          Connections
        </h2>
        <span className="font-mono text-xs text-muted-foreground">{rows.length}</span>
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Permissions</TableHead>
            <TableHead>Minted by</TableHead>
            <TableHead>Last used</TableHead>
            <TableHead className="w-24" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((connection) => (
            <ConnectionRow key={connection.signer_pubkey} slug={slug} connection={connection} />
          ))}
        </TableBody>
      </Table>
    </Card>
  );
}

function ConnectionRow({ slug, connection }: { slug: string; connection: BunkerConnection }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<PermissionDraft | undefined>();

  const update = useUpdateConnection();
  const revoke = useRevokeConnection();
  const { toast } = useToast();

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

  const save = async () => {
    if (!draft) return;
    await run(
      () =>
        update.mutateAsync({
          slug,
          signerPubkey: connection.signer_pubkey,
          patch: { permissions: draftToSpecs(draft) },
        }),
      'Permissions updated',
    );
    setEditing(false);
    setDraft(undefined);
  };

  return (
    <>
      <TableRow>
        <TableCell>
          <div className="space-y-1">
            <div className="font-medium">{connection.name}</div>
            <div className="flex items-center gap-2">
              <Badge variant={connection.connected ? 'secondary' : 'outline'} className="text-[10px]">
                {connection.connected ? 'Connected' : 'Awaiting client'}
              </Badge>
              {connection.last_error && (
                <span className="text-xs text-destructive">{connection.last_error}</span>
              )}
            </div>
          </div>
        </TableCell>

        <TableCell>
          <div className="flex flex-wrap gap-1">
            {connection.permissions.length ? (
              connection.permissions.map((spec) => (
                <Badge key={spec} variant="outline" className="text-[10px]">
                  {permissionLabel(spec)}
                </Badge>
              ))
            ) : (
              <span className="text-xs text-muted-foreground">Nothing granted</span>
            )}
          </div>
        </TableCell>

        <TableCell>
          <PubkeyLabel pubkey={connection.created_by} />
          <div className="text-xs text-muted-foreground">{formatWhen(connection.created_at)}</div>
        </TableCell>

        <TableCell className="text-sm text-muted-foreground">
          {connection.last_used_at ? formatWhen(connection.last_used_at) : 'Never'}
        </TableCell>

        <TableCell>
          <div className="flex justify-end gap-1">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setDraft(specsToDraft(connection.permissions));
                setEditing(!editing);
              }}
            >
              {editing ? 'Close' : 'Edit'}
            </Button>

            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="ghost" size="sm" aria-label={`Revoke ${connection.name}`}>
                  <Trash2 className="h-4 w-4 text-destructive" />
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Revoke {connection.name}?</AlertDialogTitle>
                  <AlertDialogDescription>
                    Its client stops being able to sign immediately, and is not told. Nothing else that signs
                    through this bunker is affected.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Keep it</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={() =>
                      run(
                        () => revoke.mutateAsync({ slug, signerPubkey: connection.signer_pubkey }),
                        'Connection revoked',
                      )
                    }
                  >
                    Revoke
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </TableCell>
      </TableRow>

      {editing && draft && (
        <TableRow>
          <TableCell colSpan={5} className="bg-muted/30">
            <div className="space-y-4 py-2">
              <PermissionPicker value={draft} onChange={setDraft} idPrefix={connection.signer_pubkey.slice(0, 8)} />
              <Button size="sm" onClick={save} disabled={update.isPending}>
                {update.isPending ? 'Saving…' : 'Save permissions'}
              </Button>
            </div>
          </TableCell>
        </TableRow>
      )}
    </>
  );
}

function formatWhen(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString();
}

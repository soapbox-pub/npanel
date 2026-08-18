import { useSeoMeta } from '@unhead/react';
import { ShieldCheck } from 'lucide-react';
import { Link } from 'react-router-dom';

import { AddBunkerDialog } from '@/components/AddBunkerDialog';
import { BunkerStatusBadge } from '@/components/bunker/BunkerStatusBadge';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useBunkers } from '@/hooks/useNpanel';

/** Identities this server can sign for without holding their keys. */
export function BunkersPage() {
  useSeoMeta({ title: 'Bunkers — npanel' });

  const bunkers = useBunkers();
  const rows = bunkers.data;

  const unavailable = bunkers.error instanceof Error && bunkers.error.message.includes('SECRET_KEY');

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Bunkers</h1>
          <p className="text-sm text-muted-foreground">
            Sign as someone without their key ever reaching this server. npanel asks the app that holds it.
          </p>
        </div>
        {!unavailable && <AddBunkerDialog />}
      </div>

      {bunkers.isLoading ? (
        <Skeleton className="h-48 w-full" />
      ) : unavailable ? (
        <Card className="border-dashed">
          <CardContent className="space-y-3 px-8 py-14 text-center">
            <p className="text-sm font-medium">Bunkers are switched off on this server</p>
            <p className="mx-auto max-w-md text-sm text-muted-foreground">
              {bunkers.error instanceof Error ? bunkers.error.message : ''} Generate one with{' '}
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">openssl rand -hex 32</code> and
              keep it somewhere other than the database backup.
            </p>
          </CardContent>
        </Card>
      ) : bunkers.isError ? (
        <Card className="border-t-2 border-t-destructive">
          <CardContent className="py-6 text-sm text-destructive">
            {bunkers.error instanceof Error ? bunkers.error.message : 'Failed to load bunkers.'}
          </CardContent>
        </Card>
      ) : !rows?.length ? (
        <Card className="border-dashed">
          <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
            <div className="rounded-full bg-accent p-3 text-accent-foreground">
              <ShieldCheck className="size-6" />
            </div>
            <p className="max-w-sm text-sm text-muted-foreground">
              No bunkers yet. Add one, then connect it to the signer app holding that identity's key.
            </p>
          </CardContent>
        </Card>
      ) : (
        <Card className="gap-0 overflow-hidden py-0">
          <div className="flex items-center justify-between gap-3 border-b bg-muted/50 px-6 py-2.5">
            <h2 className="font-mono text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
              Your bunkers
            </h2>
            <span className="font-mono text-xs text-muted-foreground">{rows.length}</span>
          </div>

          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Signs as</TableHead>
                <TableHead>Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((bunker) => (
                <TableRow key={bunker.slug}>
                  <TableCell>
                    <Link to={`/bunkers/${bunker.slug}`} className="font-medium hover:underline">
                      {bunker.name}
                    </Link>
                    {!bunker.enabled && (
                      <Badge variant="outline" className="ml-2 text-[10px]">
                        Off
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell>
                    {bunker.user_pubkey ? (
                      <PubkeyLabel pubkey={bunker.user_pubkey} />
                    ) : (
                      <span className="text-sm text-muted-foreground">Not connected yet</span>
                    )}
                  </TableCell>
                  <TableCell>
                    <BunkerStatusBadge status={bunker.status} error={bunker.error} />
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

export default BunkersPage;

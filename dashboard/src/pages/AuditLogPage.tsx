import { useSeoMeta } from '@unhead/react';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useAuditLog } from '@/hooks/useNpanel';

/** Every configuration change, most recent first. Admin only. */
export function AuditLogPage() {
  useSeoMeta({ title: 'Audit log — npanel' });

  const entries = useAuditLog();

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Audit log</h1>
        <p className="text-sm text-muted-foreground">
          Settings, user, and site changes, with who made them.
        </p>
      </div>

      {entries.isLoading ? (
        <Skeleton className="h-64 w-full" />
      ) : !entries.data?.length ? (
        <Card>
          <CardContent className="py-10 text-center text-sm text-muted-foreground">
            Nothing has been changed yet.
          </CardContent>
        </Card>
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>When</TableHead>
                <TableHead>Who</TableHead>
                <TableHead>Action</TableHead>
                <TableHead>Target</TableHead>
                <TableHead>Change</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.data.map((entry) => (
                <TableRow key={entry.id}>
                  <TableCell className="whitespace-nowrap text-sm text-muted-foreground">
                    {new Date(entry.created_at * 1000).toLocaleString()}
                  </TableCell>
                  <TableCell>
                    <PubkeyLabel pubkey={entry.actor_pubkey} compact />
                  </TableCell>
                  <TableCell>
                    <Badge variant="outline">{entry.action}</Badge>
                  </TableCell>
                  <TableCell className="max-w-40 truncate text-sm">{entry.target ?? '—'}</TableCell>
                  <TableCell className="max-w-80 truncate font-mono text-xs text-muted-foreground">
                    {format(entry.old_value)} → {format(entry.new_value)}
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

function format(value: unknown): string {
  if (value === null || value === undefined) return '—';
  return typeof value === 'string' ? value : JSON.stringify(value);
}

export default AuditLogPage;

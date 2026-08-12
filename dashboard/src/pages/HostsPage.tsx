import { useSeoMeta } from '@unhead/react';
import { ExternalLink, Globe } from 'lucide-react';
import { Link } from 'react-router-dom';

import { AddHostDialog } from '@/components/AddHostDialog';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { StatusBadge } from '@/components/StatusBadge';
import { Card, CardContent } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useHosts } from '@/hooks/useNpanel';
import { NAMED_SITE_KIND } from '@shared/nsite-address';

/** The hostname table: npanel's DNS-editor-like home screen. */
export function HostsPage() {
  useSeoMeta({ title: 'Hostnames — npanel' });

  const hosts = useHosts();

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">Hostnames</h1>
          <p className="text-sm text-muted-foreground">
            Each hostname serves one nsite, mirrored locally from Nostr and Blossom.
          </p>
        </div>
        <AddHostDialog />
      </div>

      {hosts.isLoading ? (
        <Skeleton className="h-48 w-full" />
      ) : hosts.isError ? (
        <Card className="border-t-2 border-t-destructive">
          <CardContent className="py-6 text-sm text-destructive">
            {hosts.error instanceof Error ? hosts.error.message : 'Failed to load hostnames.'}
          </CardContent>
        </Card>
      ) : !hosts.data?.length ? (
        <Card className="border-dashed">
          <CardContent className="flex flex-col items-center gap-3 px-8 py-14 text-center">
            <div className="rounded-full bg-accent p-3 text-accent-foreground">
              <Globe className="size-6" />
            </div>
            <p className="max-w-sm text-sm text-muted-foreground">
              No hostnames yet. Point a DNS record at this server and add it here.
            </p>
          </CardContent>
        </Card>
      ) : (
        <Card className="gap-0 overflow-hidden py-0">
          <div className="flex items-center justify-between gap-3 border-b bg-muted/50 px-4 py-2.5">
            <h2 className="font-mono text-[11px] font-semibold uppercase tracking-widest text-muted-foreground">
              Mapped hostnames
            </h2>
            <span className="font-mono text-xs text-muted-foreground">{hosts.data.length}</span>
          </div>
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead>Hostname</TableHead>
                <TableHead>Site</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="text-right">Files</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {hosts.data.map((host) => (
                <TableRow key={host.id}>
                  <TableCell>
                    <div className="flex items-center gap-2">
                      <Link
                        to={`/hosts/${host.id}`}
                        className="font-mono text-sm font-medium hover:text-primary hover:underline"
                      >
                        {host.hostname}
                      </Link>
                      <a
                        href={`https://${host.hostname}`}
                        target="_blank"
                        rel="noreferrer noopener"
                        className="text-muted-foreground transition-colors hover:text-primary"
                        aria-label={`Open ${host.hostname}`}
                      >
                        <ExternalLink className="size-3.5" />
                      </a>
                    </div>
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-col gap-0.5">
                      <PubkeyLabel pubkey={host.pubkey} />
                      <span className="text-xs text-muted-foreground">
                        {host.kind === NAMED_SITE_KIND ? host.identifier : 'root site'}
                      </span>
                    </div>
                  </TableCell>
                  <TableCell>
                    <StatusBadge host={host} />
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm text-muted-foreground">
                    {host.active ? host.active.files_total : '—'}
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

export default HostsPage;

import { useSeoMeta } from '@unhead/react';
import { ExternalLink } from 'lucide-react';
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
      <div className="flex items-center justify-between gap-4">
        <div>
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
        <Card>
          <CardContent className="py-6 text-sm text-destructive">
            {hosts.error instanceof Error ? hosts.error.message : 'Failed to load hostnames.'}
          </CardContent>
        </Card>
      ) : !hosts.data?.length ? (
        <Card>
          <CardContent className="py-10 text-center text-sm text-muted-foreground">
            No hostnames yet. Point a DNS record at this server and add it here.
          </CardContent>
        </Card>
      ) : (
        <Card>
          <Table>
            <TableHeader>
              <TableRow>
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
                      <Link to={`/hosts/${host.id}`} className="font-medium hover:underline">
                        {host.hostname}
                      </Link>
                      <a
                        href={`https://${host.hostname}`}
                        target="_blank"
                        rel="noreferrer noopener"
                        className="text-muted-foreground hover:text-foreground"
                        aria-label={`Open ${host.hostname}`}
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
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
                  <TableCell className="text-right text-sm text-muted-foreground">
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

import { useState } from 'react';

import { DeployLogDialog } from '@/components/host/DeployLogDialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useUpdateHost, useVersions } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { HostVersion } from '@/lib/api-types';

/** Version history with rollback, the reason npanel keeps every manifest. */
export function VersionList({ hostname }: { hostname: string }) {
  const versions = useVersions(hostname);
  const updateHost = useUpdateHost();
  const { toast } = useToast();
  const [logEventId, setLogEventId] = useState<string | undefined>();

  const pin = async (eventId: string | null) => {
    try {
      await updateHost.mutateAsync({ hostname, patch: { pinned_event_id: eventId } });
      toast({
        title: eventId ? 'Rolled back' : 'Following the latest version',
        description: eventId
          ? 'This site now serves the pinned version.'
          : 'New deploys will go live automatically again.',
      });
    } catch (error) {
      toast({
        title: "Couldn't change the version",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  if (versions.isLoading) return <Skeleton className="h-40 w-full" />;

  if (versions.isError) {
    return (
      <p className="text-sm text-destructive">
        {versions.error instanceof Error ? versions.error.message : 'Failed to load versions.'}
      </p>
    );
  }

  if (!versions.data?.versions.length) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        No manifest has been fetched for this site yet.
      </p>
    );
  }

  const { active_event_id: activeId, pinned_event_id: pinnedId } = versions.data;

  return (
    <>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Published</TableHead>
            <TableHead>State</TableHead>
            <TableHead>Files</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {versions.data.versions.map((version) => (
            <TableRow key={version.event_id}>
              <TableCell>
                <div className="flex flex-col">
                  <span>{new Date(version.created_at * 1000).toLocaleString()}</span>
                  <span className="font-mono text-xs text-muted-foreground">
                    {version.event_id.slice(0, 12)}…
                  </span>
                </div>
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap items-center gap-1.5">
                  <VersionState version={version} />
                  {version.event_id === activeId && <Badge>Serving</Badge>}
                  {version.event_id === pinnedId && <Badge variant="secondary">Pinned</Badge>}
                </div>
              </TableCell>
              <TableCell className="text-sm text-muted-foreground">
                {version.status === 'ready'
                  ? version.files_total
                  : `${version.files_ready}/${version.files_total}`}
              </TableCell>
              <TableCell className="text-right">
                <div className="flex justify-end gap-2">
                  <Button variant="ghost" size="sm" onClick={() => setLogEventId(version.event_id)}>
                    Log
                  </Button>
                  {version.event_id === pinnedId ? (
                    <Button variant="outline" size="sm" onClick={() => pin(null)}>
                      Unpin
                    </Button>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={version.status !== 'ready'}
                      onClick={() => pin(version.event_id)}
                      title={version.status === 'ready' ? undefined : 'This version is not fully downloaded'}
                    >
                      {version.event_id === activeId ? 'Pin' : 'Roll back'}
                    </Button>
                  )}
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

      <DeployLogDialog hostname={hostname} eventId={logEventId} onClose={() => setLogEventId(undefined)} />
    </>
  );
}

function VersionState({ version }: { version: HostVersion }) {
  switch (version.status) {
    case 'ready':
      return <Badge variant="outline">Ready</Badge>;
    case 'failed':
      return <Badge variant="destructive">Failed</Badge>;
    default:
      return <Badge variant="secondary">Downloading</Badge>;
  }
}

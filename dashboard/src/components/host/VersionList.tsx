import { useState } from 'react';

import { Section } from '@/components/Grouped';
import { DeployLogDialog } from '@/components/host/DeployLogDialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useUpdateHost, useVersions } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { HostVersion } from '@/lib/api-types';
import { fullDate, timeAgo } from '@/lib/format';

/** Every version the site has published, with rollback: the reason npanel keeps them all. */
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
        description: eventId ? 'New deploys wait until you unpin.' : 'New deploys go live on their own again.',
      });
    } catch (error) {
      toast({
        title: "Couldn't change the version",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  if (versions.isLoading) return <Skeleton className="h-48 w-full rounded-xl" />;

  if (versions.isError) {
    return (
      <p className="text-sm text-destructive">
        {versions.error instanceof Error ? versions.error.message : "Couldn't load the versions."}
      </p>
    );
  }

  if (!versions.data?.versions.length) {
    return <p className="py-12 text-center text-muted-foreground">Nothing has been published yet.</p>;
  }

  const { active_event_id: activeId, pinned_event_id: pinnedId } = versions.data;

  return (
    <>
      <Section
        footer={pinnedId ? 'Pinned: new deploys are downloaded but not served until you unpin.' : undefined}
      >
        {versions.data.versions.map((version) => (
          <div key={version.event_id} className="pl-4">
            <div data-row className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t py-3 pr-4">
              <div className="min-w-0 flex-1 basis-40">
                <p className="text-sm" title={fullDate(version.created_at)}>
                  {timeAgo(version.created_at)}
                </p>
                <p className="truncate font-mono text-xs text-muted-foreground">
                  {version.event_id.slice(0, 12)} · {files(version)}
                </p>
              </div>

              <div className="flex flex-wrap items-center gap-1.5">
                <VersionState version={version} />
                {version.event_id === activeId && <Badge>Serving</Badge>}
                {version.event_id === pinnedId && <Badge className="bg-warning/15 text-warning">Pinned</Badge>}
              </div>

              <div className="flex items-center gap-1">
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
                    title={version.status === 'ready' ? undefined : "This version isn't fully downloaded"}
                  >
                    {version.event_id === activeId ? 'Pin' : 'Roll Back'}
                  </Button>
                )}
              </div>
            </div>
          </div>
        ))}
      </Section>

      <DeployLogDialog hostname={hostname} eventId={logEventId} onClose={() => setLogEventId(undefined)} />
    </>
  );
}

function files(version: HostVersion): string {
  if (version.status === 'ready') return `${version.files_total} ${version.files_total === 1 ? 'file' : 'files'}`;
  return `${version.files_ready} of ${version.files_total} files`;
}

function VersionState({ version }: { version: HostVersion }) {
  switch (version.status) {
    case 'ready':
      return null;
    case 'failed':
      return <Badge variant="destructive">Failed</Badge>;
    default:
      return <Badge variant="secondary">Downloading</Badge>;
  }
}

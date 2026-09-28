import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Skeleton } from '@/components/ui/skeleton';
import { useDeployLog } from '@/hooks/useNpanel';
import { cn } from '@/lib/utils';

interface DeployLogDialogProps {
  hostname: string;
  eventId: string | undefined;
  onClose(): void;
}

const LEVEL_STYLES: Record<string, string> = {
  debug: 'text-muted-foreground',
  info: 'text-foreground',
  warn: 'text-warning',
  error: 'text-destructive',
};

/** Why a deploy succeeded or failed, straight from the sync worker. */
export function DeployLogDialog({ hostname, eventId, onClose }: DeployLogDialogProps) {
  const log = useDeployLog(hostname, eventId);

  return (
    <Dialog open={!!eventId} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Deploy Log</DialogTitle>
          <DialogDescription className="font-mono text-xs break-all">{eventId}</DialogDescription>
        </DialogHeader>

        {log.isLoading ? (
          <Skeleton className="h-64 w-full rounded-xl" />
        ) : log.isError ? (
          <p className="text-sm text-destructive">
            {log.error instanceof Error ? log.error.message : "Couldn't load the log."}
          </p>
        ) : (
          <>
            {log.data?.version.error && (
              <p className="rounded-xl bg-destructive/10 p-3 text-sm">
                {log.data.version.error}
              </p>
            )}

            <ScrollArea className="h-96 rounded-xl bg-muted">
              <ul className="divide-y divide-border/60 font-mono text-xs">
                {log.data?.entries.map((entry) => (
                  <li key={entry.id} className="flex gap-3 px-3 py-2">
                    <span className="shrink-0 text-muted-foreground tabular-nums">
                      {new Date(entry.created_at * 1000).toLocaleTimeString()}
                    </span>
                    <span className={cn('shrink-0 font-medium uppercase', LEVEL_STYLES[entry.level])}>
                      {entry.level}
                    </span>
                    <span className="min-w-0">
                      <span className="font-mono">{entry.msg}</span>
                      {entry.fields && (
                        <span className="ml-2 break-all text-muted-foreground">
                          {Object.entries(entry.fields)
                            .map(([key, value]) => `${key}=${value}`)
                            .join(' ')}
                        </span>
                      )}
                    </span>
                  </li>
                ))}
                {!log.data?.entries.length && (
                  <li className="px-3 py-6 text-center text-muted-foreground">Nothing logged yet.</li>
                )}
              </ul>
            </ScrollArea>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

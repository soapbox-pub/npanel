import { Section } from '@/components/Grouped';
import { Badge } from '@/components/ui/badge';
import { Skeleton } from '@/components/ui/skeleton';
import { useFiles } from '@/hooks/useNpanel';
import { formatBytes } from '@/lib/format';

/**
 * The files the site serves, and whether each is stored here.
 *
 * That is the host's build rather than the served version's own manifest: with
 * earlier deploys kept it holds their paths too, and npanel adds or rewrites
 * some files itself. Both are marked, or the list would disagree with the
 * version it describes.
 */
export function FileList({ hostname }: { hostname: string }) {
  const files = useFiles(hostname);

  if (files.isLoading) return <Skeleton className="h-48 w-full rounded-xl" />;

  if (files.isError) {
    return (
      <p className="py-12 text-center text-muted-foreground">
        {files.error instanceof Error ? files.error.message : "Couldn't load the files."}
      </p>
    );
  }

  if (!files.data?.files.length) {
    return <p className="py-12 text-center text-muted-foreground">No files are being served.</p>;
  }

  const total = files.data.files.reduce((sum, file) => sum + (file.size ?? 0), 0);

  return (
    <Section footer={`${files.data.files.length} files · ${formatBytes(total)}`}>
      {files.data.files.map((file) => (
        <div key={file.path} className="pl-4">
          <div data-row className="flex items-center gap-3 border-t py-2.5 pr-4">
            <span className="min-w-0 flex-1 truncate font-mono text-[13px]" title={`${file.path}\n${file.sha256}`}>
              {file.path}
            </span>
            {file.inherited && <Badge variant="secondary">Earlier deploy</Badge>}
            {file.source !== 'site' && (
              <Badge variant="secondary">{file.source === 'generated' ? 'Added by npanel' : 'Edited by npanel'}</Badge>
            )}
            {file.stored ? (
              <span className="w-16 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
                {formatBytes(file.size ?? 0)}
              </span>
            ) : (
              <Badge variant="destructive">Missing</Badge>
            )}
          </div>
        </div>
      ))}
    </Section>
  );
}

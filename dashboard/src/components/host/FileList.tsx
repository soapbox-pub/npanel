import { Badge } from '@/components/ui/badge';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useFiles } from '@/hooks/useNpanel';
import { formatBytes } from '@/lib/format';

/**
 * The files the site serves, with whether each blob is stored locally.
 *
 * With merged deployments that is more than the served version's own manifest,
 * so a path held over from an earlier deploy is marked as such — otherwise the
 * list would disagree with the version it says it is describing.
 */
export function FileList({ hostname }: { hostname: string }) {
  const files = useFiles(hostname);

  if (files.isLoading) return <Skeleton className="h-40 w-full" />;

  if (files.isError) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {files.error instanceof Error ? files.error.message : 'Failed to load files.'}
      </p>
    );
  }

  if (!files.data?.files.length) {
    return <p className="py-8 text-center text-sm text-muted-foreground">This version has no files.</p>;
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Path</TableHead>
          <TableHead>Hash</TableHead>
          <TableHead className="text-right">Size</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {files.data.files.map((file) => (
          <TableRow key={file.path}>
            <TableCell className="font-mono text-xs">
              {file.path}
              {file.inherited && (
                <Badge variant="secondary" className="ml-2 font-sans">
                  Earlier deploy
                </Badge>
              )}
            </TableCell>
            <TableCell className="font-mono text-xs text-muted-foreground">
              {file.sha256.slice(0, 12)}…
            </TableCell>
            <TableCell className="text-right text-sm">
              {file.stored ? formatBytes(file.size ?? 0) : <Badge variant="destructive">Missing</Badge>}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

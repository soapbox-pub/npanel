import { useSeoMeta } from '@unhead/react';
import { useParams } from 'react-router-dom';

import { RowIcon } from '@/components/Grouped';
import { EnvEditor } from '@/components/host/EnvEditor';
import { FileList } from '@/components/host/FileList';
import { GrantsEditor } from '@/components/host/GrantsEditor';
import { Nip05Editor } from '@/components/host/Nip05Editor';
import { NsiteSettings, PrivacySettings, SeoSettings, ServingSettings } from '@/components/host/SiteSettings';
import { VersionList } from '@/components/host/VersionList';
import { PageHeader } from '@/components/PageHeader';
import { Skeleton } from '@/components/ui/skeleton';
import { useHost, useMe } from '@/hooks/useNpanel';
import type { Host } from '@/lib/api-types';
import { type SiteSectionId, findSiteSection } from '@/lib/site-sections';
import NotFound from '@/pages/NotFound';

/** One page of a site's settings, opened from the list on the site's own page. */
export function HostSectionPage() {
  const params = useParams();
  const host = useHost(params.hostname);
  const me = useMe();
  const section = findSiteSection(params.section);

  const name = host.data?.preview.title ?? params.hostname ?? '';
  useSeoMeta({ title: section ? `${section.label} — ${name} — npanel` : 'npanel' });

  if (!section || (section.adminOnly && me.data?.role !== 'admin')) return <NotFound />;

  const back = { to: `/hosts/${params.hostname}`, label: name };

  if (host.isLoading) return <Skeleton className="h-64 w-full rounded-2xl" />;

  if (host.isError || !host.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={{ to: '/', label: 'Sites' }} title="Site not found" />
        <p className="text-muted-foreground">
          {host.error instanceof Error ? host.error.message : 'There is no site by that name.'}
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-8">
      <PageHeader
        back={back}
        icon={
          <RowIcon color={section.color} className="size-10 rounded-[10px] [&_svg]:size-6">
            <section.icon />
          </RowIcon>
        }
        title={section.label}
      />
      <Content id={section.id} host={host.data} />
    </div>
  );
}

function Content({ id, host }: { id: SiteSectionId; host: Host }) {
  switch (id) {
    case 'nsite':
      return <NsiteSettings host={host} />;
    case 'serving':
      return <ServingSettings host={host} />;
    case 'privacy':
      return <PrivacySettings host={host} />;
    case 'seo':
      return <SeoSettings host={host} />;
    case 'access':
      return <GrantsEditor hostname={host.hostname} />;
    case 'versions':
      return <VersionList hostname={host.hostname} />;
    case 'files':
      return <FileList hostname={host.hostname} />;
    case 'nip05':
      return <Nip05Editor hostname={host.hostname} />;
    case 'env':
      return <EnvEditor hostname={host.hostname} />;
  }
}

export default HostSectionPage;

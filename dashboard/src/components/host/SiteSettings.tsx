import { useState } from 'react';

import { AddressInput } from '@/components/AddressInput';
import { CertificateRow } from '@/components/CertificateRow';
import { CopyButton } from '@/components/CopyButton';
import { DnsRecords } from '@/components/DnsRecords';
import { Row, Section, ToggleRow } from '@/components/Grouped';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { useUpdateHost } from '@/hooks/useNpanel';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { useToast } from '@/hooks/useToast';
import type { Host, HostPatch } from '@/lib/api-types';
import { isRootDomain } from '@shared/hostname';

/** Save one change to a host the moment a switch moves, and say how it went. */
function usePatch(hostname: string) {
  const updateHost = useUpdateHost();
  const { toast } = useToast();

  return async (values: HostPatch, success: string) => {
    try {
      await updateHost.mutateAsync({ hostname, patch: values });
      toast({ title: success });
    } catch (error) {
      toast({
        title: "Couldn't save that",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };
}

/** Which nsite the domain serves, and where else it can be reached. */
export function NsiteSettings({ host }: { host: Host }) {
  return (
    <div className="space-y-8">
      <Section footer="The site this domain serves, and who publishes it.">
        <Row
          label={<PubkeyLabel pubkey={host.pubkey} />}
          description={<span className="block truncate font-mono">{host.address}</span>}
        >
          <div className="flex items-center gap-1">
            <CopyButton value={host.address} label="Copy the nsite address" />
            <RetargetDialog host={host} />
          </div>
        </Row>
      </Section>

      {!!host.certificates?.length && (
        <Section title="HTTPS" footer="npanel gets this site's certificates itself, and renews them before they expire.">
          {host.certificates.map((certificate) => (
            <CertificateRow key={certificate.hostname} certificate={certificate} />
          ))}
        </Section>
      )}

      {host.onion && (
        <Section title="Tor" footer="Tor Browser users are offered this address on their own.">
          <Row label={<span className="block truncate font-mono">{host.onion}</span>}>
            <CopyButton value={`http://${host.onion}`} label="Copy the onion address" />
          </Row>
        </Section>
      )}
    </div>
  );
}

/** How requests to the site are answered. */
export function ServingSettings({ host }: { host: Host }) {
  const patch = usePatch(host.hostname);

  return (
    <div className="space-y-8">
      <Section>
        <ToggleRow
          id="enabled"
          label="Serve this site"
          description={host.enabled ? undefined : 'Every request gets a 404.'}
          checked={host.enabled}
          onCheckedChange={(enabled) => patch({ enabled }, enabled ? 'Site turned on' : 'Site turned off')}
        />
      </Section>

      <Section>
        <ToggleRow
          id="spa"
          label="Single-page app"
          description="Unknown paths load the app instead of a 404."
          checked={host.spa}
          onCheckedChange={(spa) => patch({ spa }, spa ? 'Serving as a single-page app' : 'Serving as a static site')}
        />
        <ToggleRow
          id="merge"
          label="Keep files from earlier deploys"
          description="Pages already open keep loading their old chunks."
          checked={host.merge_deployments}
          onCheckedChange={(merge) =>
            patch({ merge_deployments: merge }, merge ? 'Keeping earlier files' : 'Serving the latest deploy only')
          }
        />
      </Section>

      {/* Only a root domain owns its `www`; on a subdomain the switch would be a
          promise npanel has no business making about someone's zone. */}
      {isRootDomain(host.hostname) && (
        <Section footer={host.redirect_www ? 'www needs these records to reach this server.' : undefined}>
          <ToggleRow
            id="www"
            label={`Redirect www.${host.hostname}`}
            checked={host.redirect_www}
            onCheckedChange={(redirect) =>
              patch({ redirect_www: redirect }, redirect ? 'www now redirects here' : 'www no longer redirects here')
            }
          />
          {host.redirect_www && (
            <div className="px-4 pb-3">
              <DnsRecords hostname={host.hostname} www />
            </div>
          )}
        </Section>
      )}
    </div>
  );
}

/** What the site may do in a visitor's browser, and what is known about its visitors. */
export function PrivacySettings({ host }: { host: Host }) {
  const patch = usePatch(host.hostname);

  return (
    <div className="space-y-8">
      <Section
        footer={
          host.csp_policy && (
            <details>
              <summary className="cursor-pointer text-primary select-none">Show the policy</summary>
              <p className="mt-2 font-mono break-all">{host.csp_policy}</p>
            </details>
          )
        }
      >
        <ToggleRow
          id="csp"
          label="Content Security Policy"
          description={cspDescription(host)}
          checked={host.csp}
          onCheckedChange={(csp) => patch({ csp }, csp ? 'Content Security Policy on' : 'Content Security Policy off')}
        />
      </Section>

      <Section>
        <ToggleRow
          id="analytics"
          label="Analytics"
          description="Counts visits without cookies or personal data."
          checked={host.analytics}
          onCheckedChange={(analytics) =>
            patch({ analytics }, analytics ? 'Counting visits' : 'No longer counting visits')
          }
        />
      </Section>
    </div>
  );
}

/** How the site is found: by search engines, by crawlers making link previews, and on the site list. */
export function SeoSettings({ host }: { host: Host }) {
  const patch = usePatch(host.hostname);

  return (
    <div className="space-y-8">
      <Section>
        <ToggleRow
          id="seo"
          label="Search engine defaults"
          description="A sitemap and canonical URLs, when the site has none."
          checked={host.seo}
          onCheckedChange={(seo) => patch({ seo }, seo ? 'Search engine defaults on' : 'Search engine defaults off')}
        />
        <ToggleRow
          id="link-previews"
          label="Link previews for Nostr pages"
          description="Crawlers opening a page like /npub… or /note… get that post's title, image and text. For sites that show Nostr content."
          checked={host.link_previews}
          onCheckedChange={(link_previews) =>
            patch({ link_previews }, link_previews ? 'Link previews on' : 'Link previews off')
          }
        />
      </Section>

      <Section>
        <ToggleRow
          id="listed"
          label="Show on the public site list"
          checked={!host.hidden}
          onCheckedChange={(listed) => patch({ hidden: !listed }, listed ? 'Site is now listed' : 'Site is now unlisted')}
        />
      </Section>
    </div>
  );
}

function cspDescription(host: Host): string {
  if (host.csp_source === 'off') return 'Off. Any script a deploy includes will run.';
  if (host.csp_source === 'site') return "Using the site's own policy, from its index.html.";
  return 'Scripts may only load from this domain.';
}

/** Point the domain at a different nsite. */
function RetargetDialog({ host }: { host: Host }) {
  const [open, setOpen] = useState(false);
  const [address, setAddress] = useState('');
  const parsed = useParsedAddress(address);
  const updateHost = useUpdateHost();
  const { toast } = useToast();

  const save = async () => {
    try {
      await updateHost.mutateAsync({ hostname: host.hostname, patch: { address } });
      toast({ title: `${host.hostname} now serves the new nsite` });
      setOpen(false);
      setAddress('');
    } catch (error) {
      toast({
        title: "Couldn't change the nsite",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="rounded-full">
          Change…
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Change nsite</DialogTitle>
          <DialogDescription>Serve a different nsite from {host.hostname}.</DialogDescription>
        </DialogHeader>
        <form
          className="space-y-5"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <AddressInput value={address} onChange={setAddress} parsed={parsed} id="retarget" />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!parsed.address || updateHost.isPending}>
              Change
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

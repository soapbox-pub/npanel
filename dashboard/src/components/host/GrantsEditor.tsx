import { Row, Section } from '@/components/Grouped';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { useGrants, useSetGrants, useUsers } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/**
 * Which staff may manage this site. Admins always may; staff see only the
 * sites granted to them, so this is what makes a staff account useful.
 *
 * Each switch saves as it's flipped, the way a Settings toggle does.
 */
export function GrantsEditor({ hostname }: { hostname: string }) {
  const users = useUsers();
  const grants = useGrants(hostname);
  const setGrants = useSetGrants();
  const { toast } = useToast();

  if (users.isLoading || grants.isLoading) return <Skeleton className="h-32 w-full rounded-xl" />;

  const granted = (grants.data ?? []).map((grant) => grant.user_pubkey);
  const staff = (users.data ?? []).filter((user) => user.role === 'staff');

  const toggle = async (pubkey: string, on: boolean) => {
    try {
      await setGrants.mutateAsync({
        hostname,
        pubkeys: on ? [...granted, pubkey] : granted.filter((value) => value !== pubkey),
      });
    } catch (error) {
      toast({
        title: "Couldn't change access",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  if (!staff.length) {
    return <p className="py-12 text-center text-muted-foreground">No staff yet. Add some on the Users page.</p>;
  }

  return (
    <Section footer="Admins can manage every site.">
      {staff.map((user) => (
        <Row key={user.pubkey} label={<PubkeyLabel pubkey={user.pubkey} />} htmlFor={`grant-${user.pubkey}`}>
          <Switch
            id={`grant-${user.pubkey}`}
            checked={granted.includes(user.pubkey)}
            disabled={setGrants.isPending}
            onCheckedChange={(on) => toggle(user.pubkey, on)}
          />
        </Row>
      ))}
    </Section>
  );
}

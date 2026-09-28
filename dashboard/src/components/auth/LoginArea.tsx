import { useState } from 'react';
import { Button } from '@/components/ui/button.tsx';
import AuthDialog from './AuthDialog';
import { QuickLoginDialog } from './QuickLoginDialog';
import { useLoggedInAccounts } from '@/hooks/useLoggedInAccounts';
import { AccountSwitcher } from './AccountSwitcher';
import { cn } from '@/lib/utils';

/** Minimal shape of the NIP-07 provider injected at `window.nostr`. */
interface Nip07Provider {
  getPublicKey(): Promise<string>;
}

function getNip07Provider(): Nip07Provider | undefined {
  if (typeof window === 'undefined' || !('nostr' in window)) return undefined;
  const provider = (window as { nostr?: unknown }).nostr;
  if (
    provider &&
    typeof (provider as Nip07Provider).getPublicKey === 'function'
  ) {
    return provider as Nip07Provider;
  }
  return undefined;
}

export interface LoginAreaProps {
  className?: string;
  /** Show the signed-in account's name, not only its picture. */
  showName?: boolean;
}

export function LoginArea({ className, showName }: LoginAreaProps) {
  const { currentUser } = useLoggedInAccounts();
  const [authDialogOpen, setAuthDialogOpen] = useState(false);
  const [quickLoginPubkey, setQuickLoginPubkey] = useState<string | null>(null);

  const handleJoin = async () => {
    const provider = getNip07Provider();
    if (provider) {
      try {
        const pubkey = await provider.getPublicKey();
        if (pubkey) {
          setQuickLoginPubkey(pubkey);
          return;
        }
      } catch {
        // Extension declined or errored — fall through to the full dialog.
      }
    }
    setAuthDialogOpen(true);
  };

  return (
    <div className={cn('inline-flex items-center justify-center', className)}>
      {currentUser ? (
        <AccountSwitcher showName={showName} onAddAccountClick={() => setAuthDialogOpen(true)} />
      ) : (
        <Button onClick={handleJoin} className="rounded-full px-5">
          Sign in
        </Button>
      )}

      {quickLoginPubkey && (
        <QuickLoginDialog
          isOpen={quickLoginPubkey !== null}
          pubkey={quickLoginPubkey}
          onClose={() => setQuickLoginPubkey(null)}
          onOtherLogin={() => {
            setQuickLoginPubkey(null);
            setAuthDialogOpen(true);
          }}
        />
      )}

      <AuthDialog
        isOpen={authDialogOpen}
        onClose={() => setAuthDialogOpen(false)}
      />
    </div>
  );
}

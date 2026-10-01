import { ArrowLeftRight, ChevronsUpDown, LogOut, UserPlus } from 'lucide-react';

import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.tsx';
import { Skeleton } from '@/components/ui/skeleton.tsx';
import { type Account, useLoggedInAccounts } from '@/hooks/useLoggedInAccounts';
import { useServer } from '@/hooks/useServer';
import { cn } from '@/lib/utils';

interface AccountSwitcherProps {
  onAddAccountClick: () => void;
  /** Show the account's name beside its picture, as a sidebar has room to. */
  showName?: boolean;
}

function displayName(account: Account): string {
  return account.metadata.name ?? 'Anonymous';
}

/**
 * An account's picture, or its initial on grey the way Contacts draws someone
 * without a photo. A skeleton while the profile is still loading, so no
 * placeholder initial flashes up first.
 */
function AccountAvatar({ account, pending, className }: { account: Account; pending: boolean; className?: string }) {
  return (
    <Avatar className={cn('size-8', className)}>
      <AvatarImage src={account.metadata.picture} alt="" />
      <AvatarFallback className="bg-linear-to-b from-[#a8a8ad] to-[#86868b] text-[0.8em] font-semibold text-white">
        {pending ? <Skeleton className="size-full rounded-full" /> : displayName(account).charAt(0).toUpperCase()}
      </AvatarFallback>
    </Avatar>
  );
}

export function AccountSwitcher({ onAddAccountClick, showName }: AccountSwitcherProps) {
  const { currentUser, otherUsers, isLoading, setLogin, removeLogin } = useLoggedInAccounts();
  const server = useServer();

  if (!currentUser) return null;

  // While the metadata query is in-flight and we don't yet have a name,
  // we don't want to flash a generated animal name / its first letter.
  const isCurrentUserPending = isLoading && !currentUser.metadata.name;

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        {showName ? (
          <button
            type="button"
            className="group flex h-9 w-full min-w-0 items-center gap-2 rounded-lg px-1.5 text-left transition-colors hover:bg-foreground/[0.06] focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden data-[state=open]:bg-foreground/[0.06]"
          >
            <AccountAvatar account={currentUser} pending={isCurrentUserPending} className="size-6 text-xs" />
            <span className="min-w-0 flex-1 truncate text-[13px] font-medium">
              {isCurrentUserPending ? <Skeleton className="h-3.5 w-20" /> : displayName(currentUser)}
            </span>
            <ChevronsUpDown
              className="size-3.5 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100 group-data-[state=open]:opacity-100"
              aria-hidden
            />
          </button>
        ) : (
          <button
            type="button"
            aria-label="Account"
            className="rounded-full focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
          >
            <AccountAvatar account={currentUser} pending={isCurrentUserPending} className="text-sm" />
          </button>
        )}
      </DropdownMenuTrigger>

      <DropdownMenuContent align={showName ? 'start' : 'end'} side={showName ? 'top' : 'bottom'} className="w-60">
        <div className="flex items-center gap-2.5 p-2">
          <AccountAvatar account={currentUser} pending={isCurrentUserPending} className="text-sm" />
          <p className="min-w-0 flex-1 truncate text-sm font-medium">
            {isCurrentUserPending ? <Skeleton className="h-4 w-24" /> : displayName(currentUser)}
          </p>
        </div>

        {otherUsers.map((user) => (
          <DropdownMenuItem key={user.id} onClick={() => setLogin(user.id)} className="cursor-pointer gap-2.5 p-2">
            <AccountAvatar account={user} pending={isLoading && !user.metadata.name} className="size-6 text-xs" />
            <span className="min-w-0 flex-1 truncate">{displayName(user)}</span>
          </DropdownMenuItem>
        ))}

        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={onAddAccountClick} className="cursor-pointer gap-2 p-2">
          <UserPlus className="size-4" />
          Add Account…
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => removeLogin(currentUser.id)} className="cursor-pointer gap-2 p-2">
          <LogOut className="size-4" />
          Sign Out
        </DropdownMenuItem>
        {server.standalone && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={server.disconnect} className="cursor-pointer gap-2 p-2">
              <ArrowLeftRight className="size-4" />
              <span className="min-w-0 flex-1 truncate">Switch Server…</span>
              <span className="max-w-24 truncate text-xs text-muted-foreground">{new URL(server.origin).host}</span>
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

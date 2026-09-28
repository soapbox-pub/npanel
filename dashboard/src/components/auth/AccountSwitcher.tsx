import { ChevronDown, LogOut, UserIcon, UserPlus } from 'lucide-react';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.tsx';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx';
import { Skeleton } from '@/components/ui/skeleton.tsx';
import { useLoggedInAccounts, type Account } from '@/hooks/useLoggedInAccounts';

interface AccountSwitcherProps {
  onAddAccountClick: () => void;
  /** Show the account's name beside its picture, as a sidebar has room to. */
  showName?: boolean;
}

export function AccountSwitcher({ onAddAccountClick, showName }: AccountSwitcherProps) {
  const { currentUser, otherUsers, isLoading, setLogin, removeLogin } = useLoggedInAccounts();

  if (!currentUser) return null;

  const getDisplayName = (account: Account): string => {
    return account.metadata.name ?? 'Anonymous';
  }

  // While the metadata query is in-flight and we don't yet have a name,
  // we don't want to flash a generated animal name / its first letter.
  const isCurrentUserPending = isLoading && !currentUser.metadata.name;

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button className='flex min-w-0 items-center gap-2 h-10 p-1 pr-2.5 rounded-full hover:bg-sidebar-accent transition-colors text-current focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden data-[name]:w-full data-[name]:rounded-lg' data-name={showName || undefined}>
          <Avatar className='w-8 h-8'>
            <AvatarImage
              src={currentUser.metadata.picture}
              alt={isCurrentUserPending ? '' : getDisplayName(currentUser)}
            />
            <AvatarFallback>
              {isCurrentUserPending ? (
                <Skeleton className='size-full rounded-full' />
              ) : (
                getDisplayName(currentUser).charAt(0)
              )}
            </AvatarFallback>
          </Avatar>
          {showName && (
            <span className='min-w-0 flex-1 truncate text-left text-sm font-medium'>
              {isCurrentUserPending ? <Skeleton className='h-4 w-20' /> : getDisplayName(currentUser)}
            </span>
          )}
          <ChevronDown className='w-4 h-4 opacity-60' />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align='start' className='w-56'>
        <div className='flex items-center gap-2 p-2'>
            <Avatar className='w-8 h-8'>
              <AvatarImage
                src={currentUser.metadata.picture}
                alt={isCurrentUserPending ? '' : getDisplayName(currentUser)}
              />
              <AvatarFallback>
                {isCurrentUserPending ? (
                  <Skeleton className='size-full rounded-full' />
                ) : (
                  getDisplayName(currentUser)?.charAt(0) || <UserIcon />
                )}
              </AvatarFallback>
            </Avatar>
            <div className='flex-1 truncate'>
              {isCurrentUserPending ? (
                <Skeleton className='h-4 w-24' />
              ) : (
                <p className='text-sm font-medium'>{getDisplayName(currentUser)}</p>
              )}
            </div>
        </div>
        {otherUsers.map((user) => {
          const isPending = isLoading && !user.metadata.name;
          return (
            <DropdownMenuItem
              key={user.id}
              onClick={() => setLogin(user.id)}
              className='flex items-center gap-2 cursor-pointer p-2 rounded-md'
            >
              <Avatar className='w-8 h-8'>
                <AvatarImage
                  src={user.metadata.picture}
                  alt={isPending ? '' : getDisplayName(user)}
                />
                <AvatarFallback>
                  {isPending ? (
                    <Skeleton className='size-full rounded-full' />
                  ) : (
                    getDisplayName(user)?.charAt(0) || <UserIcon />
                  )}
                </AvatarFallback>
              </Avatar>
              <div className='flex-1 truncate'>
                {isPending ? (
                  <Skeleton className='h-4 w-24' />
                ) : (
                  <p className='text-sm font-medium'>{getDisplayName(user)}</p>
                )}
              </div>
            </DropdownMenuItem>
          );
        })}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          onClick={onAddAccountClick}
          className='flex items-center gap-2 cursor-pointer p-2 rounded-md'
        >
          <UserPlus className='w-4 h-4' />
          <span>Add account</span>
        </DropdownMenuItem>
        <DropdownMenuItem
          onClick={() => removeLogin(currentUser.id)}
          className='flex items-center gap-2 cursor-pointer p-2 rounded-md text-destructive'
        >
          <LogOut className='w-4 h-4' />
          <span>Sign out</span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

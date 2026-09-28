import { useSeoMeta } from '@unhead/react';
import { Link } from 'react-router-dom';

import { buttonVariants } from '@/components/ui/button-variants';
import { cn } from '@/lib/utils';

/** The dashboard's page for an address it has nothing at. */
const NotFound = () => {
  useSeoMeta({ title: 'Not found — npanel' });

  return (
    <div className="flex flex-col items-center gap-6 py-24 text-center">
      <p className="text-7xl font-bold tracking-tight text-muted-foreground/30">404</p>
      <div className="space-y-2">
        <h1 className="text-2xl font-bold tracking-tight">There is no page configured at this address.</h1>
        <p className="text-muted-foreground">It may have moved, or it was never here.</p>
      </div>
      <Link to="/" className={cn(buttonVariants(), 'rounded-full px-5')}>
        Go to Sites
      </Link>
    </div>
  );
};

export default NotFound;

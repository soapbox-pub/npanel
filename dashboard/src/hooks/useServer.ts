import { useContext } from 'react';

import { ServerContext, type ServerContextType } from '@/contexts/ServerContext';

/** The npanel server this dashboard talks to. */
export function useServer(): ServerContextType {
  const context = useContext(ServerContext);
  if (context === undefined) {
    throw new Error('useServer must be used within a ServerProvider');
  }
  return context;
}

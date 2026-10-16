import { lazy } from "react";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { DashboardLayout } from "@/components/DashboardLayout";
import { ScrollToTop } from "@/components/ScrollToTop";

// Each page is its own chunk, so the first load carries only the one it
// opens on: charts, the date picker, and the Public Suffix List all come
// with the pages that use them.
const AnalyticsPage = lazy(() => import("@/pages/AnalyticsPage"));
const AuditLogPage = lazy(() => import("@/pages/AuditLogPage"));
const ErrorsPage = lazy(() => import("@/pages/ErrorsPage"));
const HostDetailPage = lazy(() => import("@/pages/HostDetailPage"));
const HostSectionPage = lazy(() => import("@/pages/HostSectionPage"));
const HostsPage = lazy(() => import("@/pages/HostsPage"));
const IssuePage = lazy(() => import("@/pages/IssuePage"));
const NotFound = lazy(() => import("@/pages/NotFound"));
const RedirectDetailPage = lazy(() => import("@/pages/RedirectDetailPage"));
const RedirectsPage = lazy(() => import("@/pages/RedirectsPage"));
const SettingsPage = lazy(() => import("@/pages/SettingsPage"));
const SettingsSectionPage = lazy(() => import("@/pages/SettingsSectionPage"));
const TunnelAuthorizePage = lazy(() => import("@/pages/TunnelAuthorizePage"));
const TunnelDetailPage = lazy(() => import("@/pages/TunnelDetailPage"));
const TunnelsPage = lazy(() => import("@/pages/TunnelsPage"));
const UsersPage = lazy(() => import("@/pages/UsersPage"));

export function AppRouter() {
  return (
    <BrowserRouter>
      <ScrollToTop />
      <Routes>
        <Route element={<DashboardLayout />}>
          <Route path="/" element={<HostsPage />} />
          <Route path="/hosts/:hostname" element={<HostDetailPage />} />
          <Route path="/hosts/:hostname/analytics" element={<AnalyticsPage />} />
          <Route path="/hosts/:hostname/errors" element={<ErrorsPage />} />
          <Route path="/hosts/:hostname/errors/:id" element={<IssuePage />} />
          <Route path="/hosts/:hostname/:section" element={<HostSectionPage />} />
          <Route path="/users" element={<UsersPage />} />
          <Route path="/tunnels" element={<TunnelsPage />} />
          <Route path="/tunnels/authorize" element={<TunnelAuthorizePage />} />
          <Route path="/tunnels/:hostname" element={<TunnelDetailPage />} />
          <Route path="/redirects" element={<RedirectsPage />} />
          <Route path="/redirects/:hostname" element={<RedirectDetailPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/settings/:section" element={<SettingsSectionPage />} />
          <Route path="/audit" element={<AuditLogPage />} />
          {/* ADD ALL CUSTOM ROUTES ABOVE THE CATCH-ALL "*" ROUTE */}
          <Route path="*" element={<NotFound />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}

export default AppRouter;

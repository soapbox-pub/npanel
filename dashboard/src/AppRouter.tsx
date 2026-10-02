import { lazy } from "react";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import { DashboardLayout } from "@/components/DashboardLayout";
import { ScrollToTop } from "@/components/ScrollToTop";

// Each page is its own chunk, so the first load carries only the one it
// opens on: charts, the date picker, and the Public Suffix List all come
// with the pages that use them.
const AnalyticsPage = lazy(() => import("@/pages/AnalyticsPage"));
const AuditLogPage = lazy(() => import("@/pages/AuditLogPage"));
const HostDetailPage = lazy(() => import("@/pages/HostDetailPage"));
const HostSectionPage = lazy(() => import("@/pages/HostSectionPage"));
const HostsPage = lazy(() => import("@/pages/HostsPage"));
const NotFound = lazy(() => import("@/pages/NotFound"));
const SettingsPage = lazy(() => import("@/pages/SettingsPage"));
const SettingsSectionPage = lazy(() => import("@/pages/SettingsSectionPage"));
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
          <Route path="/hosts/:hostname/:section" element={<HostSectionPage />} />
          <Route path="/users" element={<UsersPage />} />
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

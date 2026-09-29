import { BrowserRouter, Route, Routes } from "react-router-dom";

import { DashboardLayout } from "@/components/DashboardLayout";
import { ScrollToTop } from "@/components/ScrollToTop";
import AnalyticsPage from "@/pages/AnalyticsPage";
import AuditLogPage from "@/pages/AuditLogPage";
import HostDetailPage from "@/pages/HostDetailPage";
import HostSectionPage from "@/pages/HostSectionPage";
import HostsPage from "@/pages/HostsPage";
import NotFound from "@/pages/NotFound";
import SettingsPage from "@/pages/SettingsPage";
import SettingsSectionPage from "@/pages/SettingsSectionPage";
import UsersPage from "@/pages/UsersPage";

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

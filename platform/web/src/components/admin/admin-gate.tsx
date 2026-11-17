import { Navigate, Outlet } from "react-router-dom";
import { LoadingNote, PageHeader } from "@/components/page";
import { useAdminAccess } from "@/lib/admin-access";

export function AdminGate({ scope = "any" }: { scope?: "any" | "accounts" | "universes" | "keys" | "platform" }) {
  const access = useAdminAccess();
  if (access[scope]) return <Outlet />;
  if (access.isLoading) return <LoadingNote />;
  return <>
    <PageHeader title="Admin" />
    <p role="alert">{access.error?.message ?? "You do not have permission to manage this area."}</p>
  </>;
}

export function AdminIndexRedirect() {
  const access = useAdminAccess();
  if (access.isLoading) return <LoadingNote />;
  return <Navigate to={access.accounts ? "/admin/accounts" : "/admin/universes"} replace />;
}

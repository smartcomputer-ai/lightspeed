import { Navigate, useParams } from "react-router-dom";

/// Sends an old admin URL for a universe to the same page inside the universe.
export function UniverseRedirect({ to }: { to: string }) {
  const { handle = "" } = useParams();
  return <Navigate to={`/u/${encodeURIComponent(handle)}/${to}`} replace />;
}

import { lazy } from "react";
import { artifacts } from "../module";
import { DocumentIcon } from "@lightspeed-ai/platform-web/modules/ui";
import type { UniverseWebModule } from "@lightspeed-ai/platform-web/modules";

const artifactsModule: UniverseWebModule = {
  id: artifacts.id,
  label: artifacts.name,
  icon: DocumentIcon,
  action: "read",
  routes: [
    {
      path: "",
      Page: lazy(() => import("./ArtifactsPage").then(({ ArtifactsPage }) => ({ default: ArtifactsPage }))),
    },
  ],
  workspaceAction: {
    label: "View artifacts",
    href: (handle, workspaceId) =>
      `/u/${encodeURIComponent(handle)}/artifacts?${new URLSearchParams({ workspace: workspaceId })}`,
  },
};

export default artifactsModule;

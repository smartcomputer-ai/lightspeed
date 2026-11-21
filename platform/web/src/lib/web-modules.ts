import { createContext, useContext, type ComponentType } from "react";
import type { UniverseAction } from "@lightspeed-ai/platform-shared";
import type { Universe } from "@/api";
import { useActiveUniverse } from "./universes";
import { useOrganizationModules } from "@/lib/organization-modules";

export interface UniverseModuleRoute {
  /** Relative to /u/:handle/{module id}. An empty path is the module's landing page. */
  path: string;
  Page: ComponentType;
  mobileDetail?: boolean;
}

/** Contributions supplied by a native module in this web build. */
export interface UniverseWebModule {
  id: string;
  label: string;
  icon: ComponentType;
  action: UniverseAction;
  routes: readonly UniverseModuleRoute[];
  masterDetail?: boolean;
  sessionHref?: (handle: string, sessionId: string) => string;
  workspaceAction?: { label: string; href: (handle: string, workspaceId: string) => string };
}

const BundledModules = createContext<readonly UniverseWebModule[]>([]);
export const WebModuleProvider = BundledModules.Provider;
export function useBundledModules() {
  return useContext(BundledModules);
}

/** The first path segment belongs to one host page or one module. */
const HOST_NAMESPACES = new Set(["bots", "workspaces", "blobs", "profiles", "models", "environments", "mcp-servers", "credentials", "settings"]);

/** Validate once, before the router and navigation consume the distribution. */
export function composeWebModules(entries: ReadonlyArray<{ id: string; module: UniverseWebModule }>): readonly UniverseWebModule[] {
  const used = new Set(HOST_NAMESPACES);
  for (const { id, module } of entries) {
    if (id !== module.id || !/^[a-z][a-z0-9-]*$/.test(id) || used.has(id)) {
      throw new Error(`Invalid or conflicting web module ID: ${id} (exported ${module.id})`);
    }
    used.add(id);
    const paths = new Set<string>();
    for (const { path } of module.routes) {
      if (paths.has(path) || (path && path.split("/").some(part => !part || part === "." || part === ".."))) {
        throw new Error(`Invalid or duplicate route in module ${id}: ${path}`);
      }
      paths.add(path);
    }
    if (!paths.has("")) throw new Error(`Web module ${id} needs a landing page`);
  }
  return entries.map(entry => entry.module);
}

/** Only mount contributions when this organization enabled them and the caller can use them. */
export function useAvailableWebModules(universe: Pick<Universe, "organizationId" | "actions"> | undefined) {
  const bundled = useBundledModules();
  const installations = useOrganizationModules(bundled.length ? universe?.organizationId : undefined);
  const enabled = new Set(installations.data?.modules.filter(item => item.enabled).map(item => item.id));
  return {
    modules: installations.error ? [] : bundled.filter(module => enabled.has(module.id) && universe?.actions?.includes(module.action)),
    isLoading: installations.isLoading,
    error: installations.error,
  };
}

/** Embedded session readers remain usable when the standalone Sessions page is unavailable. */
export function useSessionPageHref(handle: string, sessionId: string): string | undefined {
  const { universe } = useActiveUniverse();
  const { modules } = useAvailableWebModules(universe);
  const link = modules.find(module => module.sessionHref)?.sessionHref;
  return link?.(handle, sessionId);
}

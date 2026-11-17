import { useQuery } from "@tanstack/react-query";
import { PLATFORM_OPERATIONS, type PlatformOperationId, type PlatformOperationParams, type PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { api } from "@/api";

/** Shared native API descriptors also drive SDK generation. */
export async function platformApi<K extends PlatformOperationId>(operation: K, params: PlatformOperationParams<K>): Promise<PlatformOperationResult<K>> {
  const spec = PLATFORM_OPERATIONS[operation];
  const body = { ...params } as Record<string, unknown>;
  const path = spec.path.replace(/\{([^}]+)\}/g, (_, key: string) => {
    const value = body[key];
    delete body[key];
    if (value === undefined) throw new Error(`Missing ${key}`);
    return encodeURIComponent(String(value));
  });
  return api<PlatformOperationResult<K>>(spec.method, path, spec.method === "GET" || Object.keys(body).length === 0 ? undefined : body);
}

export function useOrganizations() {
  return useQuery({ queryKey: ["organizations"], queryFn: () => platformApi("platform/organizations/list", {}) });
}

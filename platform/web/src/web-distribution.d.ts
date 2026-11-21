declare module "virtual:lightspeed/web-module-entries" {
  const entries: ReadonlyArray<{ id: string; module: import("@/lib/web-modules").UniverseWebModule }>;
  export default entries;
}

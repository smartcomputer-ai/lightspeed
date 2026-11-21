declare module "virtual:lightspeed/web-module-entries" {
  const entries: ReadonlyArray<{ id: string; module: import("@/lib/web-modules").UniverseWebModule }>;
  export default entries;
}

declare module "virtual:lightspeed/demo-module-entries" {
  type DemoRoutes = import("@/demo/router").DemoRoutes;
  const entries: ReadonlyArray<{ id: string; module: DemoRoutes }>;
  export default entries;
}

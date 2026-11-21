/** Entry points that a bundled module can use inside the shared shell. */
export { composeWebModules, type UniverseWebModule, type UniverseModuleRoute } from "../lib/web-modules";
export { useActiveUniverse, useFeature } from "../lib/universes";
export { useActionPermissions, useDeploymentPermissions, usePermissionIdentity, useSessionClosePermission, useSessionDeletePermission } from "../lib/permissions";
export { useDocumentTitle } from "../lib/document-title";
export { useSessionControllerPresenter, type ControllerPresentationProps, type SessionControllerPresentation } from "../lib/session-controller";
export { universeWatchMeta } from "../lib/query-watch";
export { useSessionPageHref } from "../lib/web-modules";
export { useWatchConnected } from "../lib/universe-watch";
export { useCreateParam } from "../lib/create-param";

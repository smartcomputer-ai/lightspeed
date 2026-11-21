/** Shared conversation view, controls and read models for native Platform modules. */
export { SessionDetail } from "@session-ui/SessionDetail";
export { MetadataMapEditor } from "@session-ui/components/metadata-editor";
export { ProfileRetentionEditor } from "@session-ui/components/profile-retention-editor";
export { SessionActionsMenu } from "@session-ui/components/session-actions-menu";
export { SessionConfigEditor } from "@session-ui/components/session-config-editor";
export { MenuIdentity } from "@session-ui/components/session-menu-details";
export { compactSessionId, sessionMetaItems, sessionName, SubAgentDetails, subAgentsTip } from "@session-ui/components/session-meta";
export { activeEnvironmentItems } from "@session-ui/components/session-resources";
export { SetupEditorSection } from "@session-ui/components/setup-editor-section";
export { sessionCollectionMeta, sessionCollections } from "@session-ui/model/collection-watch";
export { useSessionCompaction } from "@session-ui/model/compaction";
export { sessionDraftKey } from "@session-ui/model/draft";
export { useSessionConfigEditorOptions } from "@session-ui/model/editor-options";
export { defaultEnvironmentAttachment, hasSessionFeature, resourceFeatureDisableReasons, setupResourceFeatureError } from "@session-ui/model/resource-features";

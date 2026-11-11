/// The icons components import, by role: `import { DeleteIcon } from "@/icons"`.
/// Roles are named by what they stand for rather than how they look, so a
/// concept keeps one icon everywhere. The contract is `IconSet` in
/// roles.ts; the library behind it is chosen here, at build time, so only
/// that library ships. Switching libraries is the import below.
///
/// Every icon is tagged with `data-icon-role`, which the icon scale token and
/// tests select on regardless of the library drawing it. Universe icons are
/// illustrations, not part of the library (components/universe-icon.tsx).

import { createElement } from "react";
import type { IconComponent, IconSet } from "./roles";
import { phosphor as library } from "./phosphor";

export type { IconComponent, IconSet, IconLibrary } from "./roles";

/// The active library's name, such as `lucide`.
export const ICON_LIBRARY = library.name;

function tagged(Icon: IconComponent, attribute: string, value: string): IconComponent {
  const Tagged: IconComponent = (props) => createElement(Icon, { [attribute]: value, ...props });
  Tagged.displayName = value;
  return Tagged;
}

function tagAll<T extends { [K in keyof T]: IconComponent }>(icons: T, attribute: string): T {
  return Object.fromEntries(
    (Object.entries(icons) as [string, IconComponent][]).map(([name, Icon]) => [name, tagged(Icon, attribute, name)]),
  ) as T;
}

const icons: IconSet = tagAll(library.icons, "data-icon-role");

// Every role in IconSet, in the order roles.ts declares them.
export const {
  SessionIcon, WorkspaceIcon, EnvironmentIcon, EnvironmentProviderIcon, ModelIcon, McpServerIcon, ProfileIcon,
  TemplateIcon, ChannelIcon, CredentialIcon, ApiKeyIcon, MemberIcon, UserAdminIcon, AuditLogIcon, UniversesIcon,
  SettingsIcon, FeatureOffIcon, BotIcon, BotAvatarIcon, UserIcon, SecurityIcon, SecretIcon, AppearanceIcon,
  AdminIcon, ManagedIcon, PrivateIcon, SharedIcon, SignInIcon, SignOutIcon, RevokeIcon, DisconnectIcon,
  LightThemeIcon, DarkThemeIcon, SystemThemeIcon, GitHubLogo, OpenAiLogo, AnthropicLogo, CompatibleProviderLogo,
  ToolsIcon, SubAgentIcon, CodeModeIcon, WebIcon, TimerIcon, MetadataIcon, RetentionIcon, ThinkingIcon,
  ExploreToolIcon, EditToolIcon, ExecuteToolIcon, CodeToolIcon, McpToolIcon, DelegateToolIcon, AwaitToolIcon,
  EmitToSelfIcon, MessageToolIcon, OtherToolIcon, ToolGroupIcon, BotEventIcon, ScheduleIcon, WebhookIcon,
  ChatTriggerIcon, InboxIcon, PollIcon, CommandIcon, ActivityIcon, AddIcon, AttachIcon, EditIcon, EditFilesIcon,
  DeleteIcon, ArchiveIcon, CopyIcon, CopyLinkIcon, DownloadIcon, UploadIcon, UploadFolderIcon, NewFileIcon,
  NewFolderIcon, ShareIcon, CompactIcon, ConfigureIcon, FilterIcon, SelectModeIcon, SendIcon, SubmitIcon, QueueIcon,
  SteerIcon, StopIcon, PauseIcon, ResumeIcon, RetryIcon, RotateSecretIcon, ResetIcon, ReplayIcon, RefreshIcon,
  GenerateIcon, DictateIcon, DictationUnavailableIcon, ApproveIcon, RejectIcon, RemoveIcon, CancelIcon, DismissIcon,
  MoreIcon, SearchIcon, BackIcon, GoToIcon, NewTabIcon, ExpandIcon, CollapseIcon, DropdownCaretIcon, ShowMoreIcon,
  ShowLessIcon, RowLinkIcon, SwitcherIcon, LoadingIcon, SelectedIcon, CopiedIcon, StepDoneIcon, SuccessIcon,
  WarningIcon, InfoIcon, FailedIcon, CancelledIcon, WaitingIcon, ApprovalIcon, LockedIcon, FileIcon, FolderIcon,
  DocumentIcon, UnknownFileIcon, ImageUnavailableIcon, CheckedIcon, FieldCaretIcon, SelectTriggerIcon, ScrollUpIcon,
  ScrollDownIcon, SubmenuIcon, DialogCloseIcon, ClearFieldIcon, ChipRemoveIcon, SidebarToggleIcon,
  ScrollToBottomIcon, DebugIcon, FeedbackIcon, DesignIcon, ExportCodeIcon, NotificationIcon, EmailIcon,
} = icons;

/// The icon contract: every role the app draws, named by what it stands
/// for rather than how it looks. An icon library implements it as an
/// `IconLibrary` (see lucide.ts); `index.ts` picks one at build time and
/// re-exports its roles, which is what components import from `@/icons`.
///
/// Adding a role means adding it here, to every library, and to the export
/// list in index.ts; the typecheck and icon tests catch any that are missed.

import type { ComponentType, SVGProps } from "react";

/// Any icon component a library provides.
export type IconComponent = ComponentType<SVGProps<SVGSVGElement> & { size?: number | string }>;

export interface IconSet {
  // Places and things: the sidebar sections and what they list.
  SessionIcon: IconComponent;
  WorkspaceIcon: IconComponent;
  EnvironmentIcon: IconComponent;
  EnvironmentProviderIcon: IconComponent;
  ModelIcon: IconComponent;
  McpServerIcon: IconComponent;
  ProfileIcon: IconComponent;
  TemplateIcon: IconComponent;
  ChannelIcon: IconComponent;
  CredentialIcon: IconComponent;
  ApiKeyIcon: IconComponent;
  MemberIcon: IconComponent;
  UserAdminIcon: IconComponent;
  AuditLogIcon: IconComponent;
  UniversesIcon: IconComponent;
  SettingsIcon: IconComponent;
  FeatureOffIcon: IconComponent;
  BotIcon: IconComponent;
  BotAvatarIcon: IconComponent;

  // Account and access.
  UserIcon: IconComponent;
  SecurityIcon: IconComponent;
  SecretIcon: IconComponent;
  AppearanceIcon: IconComponent;
  AdminIcon: IconComponent;
  ManagedIcon: IconComponent;
  PrivateIcon: IconComponent;
  SharedIcon: IconComponent;
  SignInIcon: IconComponent;
  SignOutIcon: IconComponent;
  RevokeIcon: IconComponent;
  DisconnectIcon: IconComponent;
  LightThemeIcon: IconComponent;
  DarkThemeIcon: IconComponent;
  SystemThemeIcon: IconComponent;
  GitHubLogo: IconComponent;
  OpenAiLogo: IconComponent;
  AnthropicLogo: IconComponent;
  CompatibleProviderLogo: IconComponent;

  // What a session can use.
  ToolsIcon: IconComponent;
  SubAgentIcon: IconComponent;
  CodeModeIcon: IconComponent;
  WebIcon: IconComponent;
  TimerIcon: IconComponent;
  MetadataIcon: IconComponent;
  RetentionIcon: IconComponent;

  // Steps in a run, by tool family.
  ThinkingIcon: IconComponent;
  ExploreToolIcon: IconComponent;
  EditToolIcon: IconComponent;
  ExecuteToolIcon: IconComponent;
  CodeToolIcon: IconComponent;
  McpToolIcon: IconComponent;
  DelegateToolIcon: IconComponent;
  AwaitToolIcon: IconComponent;
  EmitToSelfIcon: IconComponent;
  MessageToolIcon: IconComponent;
  OtherToolIcon: IconComponent;
  ToolGroupIcon: IconComponent;
  BotEventIcon: IconComponent;

  // Bot triggers and what arrives through them.
  ScheduleIcon: IconComponent;
  WebhookIcon: IconComponent;
  ChatTriggerIcon: IconComponent;
  InboxIcon: IconComponent;
  PollIcon: IconComponent;
  CommandIcon: IconComponent;
  ActivityIcon: IconComponent;

  // Actions.
  AddIcon: IconComponent;
  AttachIcon: IconComponent;
  EditIcon: IconComponent;
  EditFilesIcon: IconComponent;
  DeleteIcon: IconComponent;
  ArchiveIcon: IconComponent;
  /** Closing a session, and the mark of a closed one. */
  SessionCloseIcon: IconComponent;
  CopyIcon: IconComponent;
  CopyLinkIcon: IconComponent;
  DownloadIcon: IconComponent;
  UploadIcon: IconComponent;
  UploadFolderIcon: IconComponent;
  NewFileIcon: IconComponent;
  NewFolderIcon: IconComponent;
  ShareIcon: IconComponent;
  CompactIcon: IconComponent;
  ConfigureIcon: IconComponent;
  FilterIcon: IconComponent;
  SelectModeIcon: IconComponent;
  SendIcon: IconComponent;
  SubmitIcon: IconComponent;
  QueueIcon: IconComponent;
  SteerIcon: IconComponent;
  StopIcon: IconComponent;
  PauseIcon: IconComponent;
  ResumeIcon: IconComponent;
  RetryIcon: IconComponent;
  RotateSecretIcon: IconComponent;
  ResetIcon: IconComponent;
  ReplayIcon: IconComponent;
  RefreshIcon: IconComponent;
  GenerateIcon: IconComponent;
  DictateIcon: IconComponent;
  DictationUnavailableIcon: IconComponent;
  ApproveIcon: IconComponent;
  RejectIcon: IconComponent;
  RemoveIcon: IconComponent;
  CancelIcon: IconComponent;
  DismissIcon: IconComponent;
  MoreIcon: IconComponent;
  SearchIcon: IconComponent;

  // Navigation and disclosure.
  BackIcon: IconComponent;
  GoToIcon: IconComponent;
  NewTabIcon: IconComponent;
  ExpandIcon: IconComponent;
  /** A meta toolbar drawn as icons, offering to expand into tags (or to keep them expanded). */
  ExpandToolsIcon: IconComponent;
  /** A meta toolbar expanded into tags, offering to collapse back to icons. */
  CollapseToolsIcon: IconComponent;
  CollapseIcon: IconComponent;
  DropdownCaretIcon: IconComponent;
  ShowMoreIcon: IconComponent;
  ShowLessIcon: IconComponent;
  RowLinkIcon: IconComponent;
  SwitcherIcon: IconComponent;

  // States and feedback.
  LoadingIcon: IconComponent;
  SelectedIcon: IconComponent;
  CopiedIcon: IconComponent;
  StepDoneIcon: IconComponent;
  SuccessIcon: IconComponent;
  WarningIcon: IconComponent;
  InfoIcon: IconComponent;
  FailedIcon: IconComponent;
  CancelledIcon: IconComponent;
  WaitingIcon: IconComponent;
  ApprovalIcon: IconComponent;
  LockedIcon: IconComponent;

  // Files and media.
  FileIcon: IconComponent;
  FolderIcon: IconComponent;
  /** A folder whose contents are shown (an open row in a file list). */
  FolderOpenIcon: IconComponent;
  /** A folder whose contents are hidden (a closed row in a file list). */
  FolderClosedIcon: IconComponent;
  DocumentIcon: IconComponent;
  UnknownFileIcon: IconComponent;

  // File types, by name and extension (components/file-icon picks one).
  /** A PDF. */
  FilePdfIcon: IconComponent;
  /** A word-processor document (doc, docx, odt, rtf). */
  FileDocIcon: IconComponent;
  /** A spreadsheet (xls, xlsx, ods). */
  FileXlsIcon: IconComponent;
  /** A presentation (ppt, pptx, odp). */
  FilePptIcon: IconComponent;
  /** Plain text (txt, log). */
  FileTxtIcon: IconComponent;
  /** Markdown. */
  FileMdIcon: IconComponent;
  /** Comma- or tab-separated values. */
  FileCsvIcon: IconComponent;
  /** A PNG image. */
  FilePngIcon: IconComponent;
  /** A JPEG image. */
  FileJpgIcon: IconComponent;
  /** An SVG image. */
  FileSvgIcon: IconComponent;
  /** TypeScript. */
  FileTsIcon: IconComponent;
  /** TypeScript with JSX. */
  FileTsxIcon: IconComponent;
  /** JavaScript. */
  FileJsIcon: IconComponent;
  /** JavaScript with JSX. */
  FileJsxIcon: IconComponent;
  /** Python. */
  FilePyIcon: IconComponent;
  /** Rust. */
  FileRsIcon: IconComponent;
  /** C. */
  FileCIcon: IconComponent;
  /** C++. */
  FileCppIcon: IconComponent;
  /** C#. */
  FileCSharpIcon: IconComponent;
  /** A stylesheet (css, scss, less). */
  FileCssIcon: IconComponent;
  /** HTML. */
  FileHtmlIcon: IconComponent;
  /** A Vue component. */
  FileVueIcon: IconComponent;
  /** SQL. */
  FileSqlIcon: IconComponent;
  /** Settings (ini, env, conf, dotfiles). */
  FileIniIcon: IconComponent;
  /** A zip archive. */
  FileZipIcon: IconComponent;
  /** Source code or structured data without its own icon (json, yaml, go, shell, …). */
  FileCodeIcon: IconComponent;
  /** An image without its own icon (gif, webp, …). */
  FileImageIcon: IconComponent;
  /** Audio (mp3, wav, …). */
  FileAudioIcon: IconComponent;
  /** Video (mp4, mov, …). */
  FileVideoIcon: IconComponent;
  /** An archive without its own icon (tar, gz, 7z, …). */
  FileArchiveIcon: IconComponent;
  ImageUnavailableIcon: IconComponent;

  // Controls inside the shared ui components (components/ui).
  CheckedIcon: IconComponent;
  FieldCaretIcon: IconComponent;
  SelectTriggerIcon: IconComponent;
  ScrollUpIcon: IconComponent;
  ScrollDownIcon: IconComponent;
  SubmenuIcon: IconComponent;
  DialogCloseIcon: IconComponent;
  ClearFieldIcon: IconComponent;
  ChipRemoveIcon: IconComponent;
  SidebarToggleIcon: IconComponent;
  ScrollToBottomIcon: IconComponent;

  // Developer tools (debug mode and the UI overview).
  DebugIcon: IconComponent;
  FeedbackIcon: IconComponent;
  DesignIcon: IconComponent;
  ExportCodeIcon: IconComponent;
  NotificationIcon: IconComponent;
  EmailIcon: IconComponent;
  /// The design configurator: a token with the same value in light and dark, or different ones.
  ThemesSameIcon: IconComponent;
  ThemesDifferIcon: IconComponent;
}

/// One complete implementation of the contract.
export interface IconLibrary {
  /// Short name shown in the UI overview, such as `lucide`.
  name: string;
  icons: IconSet;
}

import { sessionCollections } from "@session-ui/model/collection-watch";
import { DeleteSessionDialog } from "@session-ui/components/delete-session-dialog";
import { useSessionCompaction } from "@session-ui/model/compaction";
import { useDictationAvailability } from "@/lib/use-dictation";
import { ShareSessionDialog, useCanShareSession, useSessionOwner } from "@session-ui/components/sharing";
import { SessionActionsMenu } from "@session-ui/components/session-actions-menu";
import { useCallback, useContext, useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import { type InfiniteData, type UseQueryResult, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { ConfigureIcon, DeleteIcon, SessionCloseIcon, SessionIcon } from "@/icons";
import {
  api,
  type SessionListPage,
  type SessionRunAccepted,
  type SessionRunApprovalsDecided,
  SessionRunCancelled,
  SessionRunSteered,
  SessionRunView,
  type SessionView,
} from "@/api";
import { Button } from "@/components/ui/button";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { useUserPreferences } from "@/lib/user-preferences";
import { SessionSettingsDialog } from "@session-ui/components/session-settings-sheet";
import { SessionHistoryLoader } from "@session-ui/components/history-loader";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
  useMessageScroller,
  useMessageScrollerScrollable,
} from "@/components/ui/message-scroller";
import { sessionDraftKey } from "@session-ui/model/draft";
import { SessionComposer, type ComposerMessage, type ComposerMode } from "@session-ui/components/composer";
import { sessionResourceItems, useSessionResources } from "@session-ui/components/session-resources";
import { hasLineage, SESSION_META, sessionMetaItems, useSessionLineage } from "@session-ui/components/session-meta";
import { MetaToolbar } from "@/components/meta-toolbar";
import { hasResources } from "@session-ui/model/session-resources";
import { findFileReference, filesByHandle } from "@/lib/file-references";
import { appHref, blobHref } from "@/lib/blob-view";
import { Switch } from "@/components/ui/switch";
import {
  ApprovalCards,
  QueuedRunsBar,
  SystemChips,
  TranscriptEntryView,
  type QueuedRunItem,
} from "@session-ui/components/transcript-view";
import { RunSectionView } from "@session-ui/components/run-section";
import { TranscriptMotionProvider } from "@session-ui/components/transcript-motion";
import { TranscriptLinksContext, type TranscriptLinks } from "@session-ui/components/tool-trace";
import { projectSubmissions, submissionReducer, type LocalSubmission } from "@session-ui/model/submissions";
import { LoadingNote } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { fetchHistory, useSessionTail } from "@session-ui/model/tail";
import {
  mediaByHandle,
  mediaHandleFor,
  runInProgress,
  type ActiveRun,
  type TranscriptEntry,
  type TranscriptMedia,
} from "@session-ui/model/transcript";
import type { SentAttachment } from "@/lib/composer-attachments";
import type { RunView } from "@lightspeed-ai/sdk";
import { postUntilAccepted } from "@session-ui/model/held-request";
import {
  queuedMessage,
  queuedRecoverySubmissionId,
  readQueuedRecovery,
  steeringWasNotSubmitted,
  storeQueuedRecovery,
  type QueuedMessageRecovery,
} from "@session-ui/model/queued-message";
import { SessionControllerPresenter, type SessionControllerPresentation } from "./controller-presentation";
import { ProviderReadinessBanner } from "@/components/provider-readiness-banner";
import { modelFromConfig, useModelDiscovery } from "@/lib/model-defaults";
import { useActionPermissions, useSessionClosePermission, useSessionDeletePermission } from "@/lib/permissions";
import { DetailHeader } from "@/components/detail-header";
import { useDocumentTitle } from "@/lib/document-title";

/// A dead workflow emits nothing, so a working session is re-read to notice it.
const SESSION_LIVENESS_REFRESH_MS = 15_000;
/// The strip of tags under a session header (the meta toolbar in full). Off
/// for now: the header beside the title shows the same items, expanding to
/// tags on hover.
const SHOW_META_STRIP = false;
const sessionMayBeRunning = (session?: Pick<SessionView, "lifecycleStatus" | "activity" | "executionFailure">) =>
  session?.lifecycleStatus !== "closed" && session?.activity !== undefined && session.activity !== "idle" && !session.executionFailure;
const EXECUTION_FAILURE_TEXT: Record<NonNullable<SessionView["executionFailure"]>, string> = {
  failed: "This session's workflow failed.",
  terminated: "This session's workflow was stopped.",
  timedOut: "This session's workflow timed out.",
  missing: "This session's workflow no longer exists.",
};

interface SessionDetailProps {
  universeId: string;
  handle: string;
  sessionId: string;
  backTo: string;
  /**
   * Rendered inside another surface (a bot's Chat tab): no back link, and a
   * controlled session takes plain input — the bot page is where its operator
   * talks to it, so there is nothing to override.
   */
  embedded?: boolean;
  /**
   * Embedded, a header of its own in place of the page's: `title` names
   * what is open (a switcher, say), beside the session's meta toolbar as
   * its page shows it (without who drives it: the surrounding page says
   * that), and `actions` its ⋯ menu.
   */
  embeddedHeader?: { title: ReactNode; actions?: ReactNode };
  /** The host supplies destinations for lineage and transcript links. */
  sessionHref: (sessionId: string) => string;
}

/** Key by sessionDraftKey: local submissions and render identities belong to this mount. */
export function SessionDetail(props: SessionDetailProps) {
  const { universeId, handle, sessionId } = props;
  const PresentController = useContext(SessionControllerPresenter);
  const session = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () =>
      api<SessionView>(
        "GET",
        `/api/ui/universes/${universeId}/sessions/${sessionId}`,
      ),
    refetchInterval: (query) => sessionMayBeRunning(query.state.data) ? SESSION_LIVENESS_REFRESH_MS : false,
  });
  return (
    <PresentController universeId={universeId} handle={handle} session={session.data}>
      {(controller) => <SessionConversation {...props} session={session} controller={controller} />}
    </PresentController>
  );
}

function SessionConversation({
  universeId, handle, sessionId, backTo, embedded = false, embeddedHeader, sessionHref,
  session, controller,
}: SessionDetailProps & {
  session: UseQueryResult<SessionView>;
  controller: SessionControllerPresentation;
}) {
  const dictation = useDictationAvailability(universeId);
  const modelDiscovery = useModelDiscovery(universeId);
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const tail = useSessionTail(universeId, sessionId);
  const title = session.data?.displayName ?? sessionId.slice(0, 24);
  // Embedded, the surrounding page names what is open.
  useDocumentTitle(embedded ? [] : [title]);
  const compaction = useSessionCompaction(universeId, sessionId, session.data);
  const owner = useSessionOwner(universeId, session.data?.access);
  const canShare = useCanShareSession(universeId, session.data?.access, Boolean(session.data?.origin));
  const [submissions, updateSubmission] = useReducer(submissionReducer, []);
  const submissionScope = JSON.stringify([universeId, sessionId]);
  const currentScope = useRef<string | null>(submissionScope);
  useEffect(() => {
    currentScope.current = submissionScope;
    return () => { currentScope.current = null; };
  }, [submissionScope]);
  const [notices, setNotices] = useState<{ id: string; text: string }[]>([]);
  const [stoppingRunId, setStoppingRunId] = useState<string | null>(null);
  const [cancellingQueued, setCancellingQueued] = useState<Set<string>>(() => new Set());
  const [steeringQueued, setSteeringQueued] = useState<string | null>(null);
  const queueActions = useRef(new Set<string>());
  const [recoveredMessages, setRecoveredMessages] = useState<QueuedMessageRecovery[]>([]);
  useEffect(() => {
    setRecoveredMessages((prev) => [
      ...prev.filter((item) => item.scope !== submissionScope), ...readQueuedRecovery(submissionScope),
    ]);
  }, [submissionScope]);
  const [followRequest, setFollowRequest] = useState(0);
  const [sendError, setSendError] = useState<string | null>(null);
  const [closeError, setCloseError] = useState<string | null>(null);
  const [closeOpen, setCloseOpen] = useState(false);
  const [shareOpen, setShareOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const permissions = useActionPermissions(universeId);
  const mayClose = useSessionClosePermission(universeId);
  const mayDelete = useSessionDeletePermission(universeId);
  const canControl = permissions.can("controlSession");
  const canConfigure = permissions.can("configureSession");
  const canStop = permissions.can("stopSession");
  const canClose = mayClose(session.data?.access);
  const canDelete = mayDelete(session.data?.access);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [decidingApproval, setDecidingApproval] = useState<{
    approvalId: string;
    decision: "approve" | "reject";
  } | null>(null);
  const [approvalError, setApprovalError] = useState<{
    approvalId: string;
    message: string;
  } | null>(null);

  const { showRunStatistics, collapseCompletedRuns, showSessionResources } = useUserPreferences();
  const resources = useSessionResources(
    universeId, session.data?.config, session.data?.activeEnvironmentId, showSessionResources && Boolean(session.data),
  );
  const entries = tail.transcript.entries;
  const loadFullText = useCallback(
    async (blobRef: string) => {
      const result = await api<{ bytesBase64: string }>(
        "GET", `/api/ui/universes/${universeId}/blobs/${encodeURIComponent(blobRef)}?resourceKind=session&resourceId=${encodeURIComponent(sessionId)}`,
      );
      return new TextDecoder().decode(Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0)));
    },
    [universeId, sessionId],
  );
  const activeRun = tail.transcript.activeRun;
  const queuedRuns = tail.transcript.queuedRuns;
  const runRevision = tail.transcript.runRevision;

  // The session view is authoritative for run state; fold it into the tail
  // whenever it arrives (forward moves only), and refresh it on every run
  // lifecycle change the tail reports so queued-run text and terminal
  // statuses stay current.
  const reconcileRuns = tail.reconcileRuns;
  const sessionRuns = session.data?.runs;
  const sessionActiveRun = session.data?.activeRun;
  const approvalRun = sessionActiveRun?.pendingApprovals?.length
    ? sessionActiveRun : sessionRuns?.find((run) => (run.pendingApprovals?.length ?? 0) > 0);
  useEffect(() => {
    if (tail.phase === "live") {
      reconcileRuns([...(sessionRuns ?? []), ...(sessionActiveRun ? [sessionActiveRun] : [])]);
    }
  }, [sessionRuns, sessionActiveRun, tail.phase, reconcileRuns]);
  const refetchSession = session.refetch;
  useEffect(() => {
    if (runRevision > 0) {
      void refetchSession();
    }
  }, [runRevision, refetchSession]);

  const submissionView = useMemo(
    () => projectSubmissions(tail.transcript, submissions),
    [tail.transcript, submissions],
  );
  const { sections: displaySections, inlinePending: pendingInTranscript, pendingRuns: resolvedPending } = submissionView;

  useEffect(() => {
    if (stoppingRunId && tail.transcript.runPhases.get(stoppingRunId) === "terminal") {
      setStoppingRunId(null);
    }
    setCancellingQueued((prev) => {
      if (prev.size === 0) {
        return prev;
      }
      const next = new Set(
        [...prev].filter((runId) => tail.transcript.runPhases.get(runId) !== "terminal"),
      );
      return next.size === prev.size ? prev : next;
    });
  }, [runRevision, tail.transcript.runPhases, stoppingRunId]);

  // The run to steer or stop: the tail's active run, or — before the tail
  // has reported it — the run the engine just accepted as running.
  const steerTargetRunId =
    activeRun?.runId ??
    resolvedPending.find(
      (message) =>
        message.runId &&
        (tail.transcript.runPhases.has(message.runId)
          ? tail.transcript.runPhases.get(message.runId) === "running"
          : message.status === "running" || message.status === "parked"),
    )?.runId ??
    null;
  const runActive = runInProgress(tail.transcript) || resolvedPending.length > 0;
  const stopping = stoppingRunId !== null && steerTargetRunId === stoppingRunId;
  const canSteer = canControl && steerTargetRunId !== null && !(activeRun?.cancelling ?? false) && !stopping;
  const queuedItems: QueuedRunItem[] = [
    ...submissionView.queuedRuns.map((run) => {
      const sent = resolvedPending.find((message) => message.runId === run.runId);
      return {
        key: sent?.id ?? run.runId,
        runId: run.runId,
        text: queuedRunText(run.runId, sessionRuns, resolvedPending),
        cancelling: cancellingQueued.has(run.runId),
        steering: steeringQueued === run.runId,
      };
    }),
    ...resolvedPending
      .filter(
        (message) =>
          submissionView.queued.has(message.id) &&
          !submissionView.queuedRuns.some(run => run.runId === message.runId),
      )
      .map((message) => ({
        key: message.id,
        runId: message.runId ?? null,
        text: message.text || attachmentSummary(message.media),
        cancelling: message.runId !== null && cancellingQueued.has(message.runId),
        steering: message.runId !== null && steeringQueued === message.runId,
      })),
  ];
  const closed = session.data?.lifecycleStatus === "closed";
  const controlled = session.data?.lifecycleControlled === true;
  const managerLabel = controller.label;
  // The session's meta items, drawn compact beside the title and full in
  // the strip under the header. The strip shows while there are resources
  // (when wanted) or a lineage to show.
  const lineage = useSessionLineage({ universeId, sessionId, origin: session.data?.origin, runRevision });
  const resourcesShown = showSessionResources && Boolean(session.data) && hasResources(resources);
  const meta = session.data ? sessionMetaItems({
    session: session.data,
    controller: controller.attribution ?? { label: managerLabel },
    lineage,
    lineageHref: sessionHref,
    resources: resourcesShown
      ? sessionResourceItems({ resources, handle, universeId, onConfigure: !embedded && canConfigure ? () => setSettingsOpen(true) : undefined })
      : [],
  }) : [];
  const showStrip = SHOW_META_STRIP && meta.length > 0 && (resourcesShown || hasLineage(lineage));
  const loadMedia = useCallback(
    async (blobRef: string, mime: string) => {
      const result = await api<{ bytesBase64: string }>(
        "GET", `/api/ui/universes/${universeId}/blobs/${encodeURIComponent(blobRef)}?resourceKind=session&resourceId=${encodeURIComponent(sessionId)}`,
      );
      const bytes = Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0));
      return new Blob([bytes], { type: mime });
    },
    [universeId, sessionId],
  );
  const transcriptMedia = useMemo(() => mediaByHandle(entries), [entries]);
  const transcriptFiles = useMemo(() => filesByHandle(entries), [entries]);
  const transcriptLinks = useMemo<TranscriptLinks>(() => {
    return {
      botName: controller.participantName,
      sessionHref,
      // A browser address: these links open in a new tab, outside the router.
      blobHref: (blobRef, hints) => {
        const path = blobHref(handle, blobRef, { ...hints, session: sessionId });
        return path && appHref(path);
      },
      navigate: (target) => void navigate(target),
      loadMedia,
      mediaByHandle: transcriptMedia,
      filesByHandle: transcriptFiles,
      fileReferenceSource: {
        universeId, sessionId,
        load: (handle, signal) => findFileReference(handle, (before) => fetchHistory(universeId, sessionId, before, signal)),
      },
    };
  }, [controller.participantName, sessionHref, handle, sessionId, navigate, loadMedia, transcriptMedia, transcriptFiles, universeId]);
  // Operator override: the engine happily admits direct runs on a controlled
  // session (they queue like any client run), so the gate here is policy,
  // not capability. Off by default because direct input bypasses the
  // manager's ingress; resets when the operator navigates away.
  const controlledGate = controlled && !embedded;
  const [directInput, setDirectInput] = useState(false);
  const queueSteerTarget = useRef<string | null>(null);
  queueSteerTarget.current = canSteer && canStop && !closed && (!controlledGate || directInput) ? steerTargetRunId : null;
  useEffect(() => {
    setDirectInput(false);
  }, [sessionId]);

  useEffect(() => {
    if (settingsOpen && !runActive) {
      void session.refetch();
    }
  }, [settingsOpen, runActive]);

  const send = async (message: ComposerMessage, mode: ComposerMode | null, submissionId: string = crypto.randomUUID()) => {
    if (!canControl) return false;
    if (currentScope.current === submissionScope) setSendError(null);
    if (mode === "steer") {
      const result = await steer(message);
      if (!result.ok && currentScope.current === submissionScope) {
        setSendError(result.error instanceof Error ? result.error.message : String(result.error));
      }
      return result.ok;
    }
    const { text } = message;
    const media = echoMedia(message.attachments);
    // The submission id doubles as the engine idempotency key: a retried
    // POST returns the original run instead of starting a second one.
    updateSubmission({ type: "send", submission: {
      id: submissionId, kind: "run", text, media, runId: null, queued: runActive,
    } });
    try {
      const accepted = await postUntilAccepted(
        () => api<SessionRunAccepted>(
          "POST",
          `/api/ui/universes/${universeId}/sessions/${sessionId}/messages`,
          {
            text,
            submissionId,
            ...(message.attachments.length ? { attachments: message.attachments.map(wireAttachment) } : {}),
            ...(canConfigure && message.options ? { options: message.options } : {}),
          },
        ),
        () => currentScope.current === submissionScope,
      );
      if (!accepted) return true;
      if (currentScope.current === submissionScope) setFollowRequest((request) => request + 1);
      updateSubmission({ type: "accepted", id: submissionId,
        runId: accepted.run.id, status: accepted.run.status });
      if (["completed", "cancelled", "failed"].includes(accepted.run.status)) void session.refetch();
      return true;
    } catch (error) {
      if (currentScope.current === submissionScope) {
        updateSubmission({ type: "withdrawn", id: submissionId });
        setSendError(error instanceof Error ? error.message : String(error));
      }
      return false;
    }
  };

  const steer = async (
    { text, attachments }: ComposerMessage, runId = steerTargetRunId,
  ): Promise<{ ok: true } | { ok: false; error: unknown }> => {
    if (!canControl) return { ok: false, error: new Error("You have read-only access to this session.") };
    if (!runId) {
      return { ok: false, error: new Error(
        "There is no run to steer yet — wait a moment and try again, or press Enter to queue it.",
      ) };
    }
    const id = crypto.randomUUID();
    if (currentScope.current === submissionScope) {
      updateSubmission({ type: "send", submission: {
        id, kind: "steering", runId, text, media: echoMedia(attachments), queued: false,
      } });
    }
    try {
      const accepted = await postUntilAccepted(
        () => api<SessionRunSteered>(
          "POST",
          `/api/ui/universes/${universeId}/sessions/${sessionId}/runs/${runId}/steer`,
          {
            text,
            submissionId: id,
            ...(attachments.length ? { attachments: attachments.map(wireAttachment) } : {}),
          },
        ),
        () => currentScope.current === submissionScope,
      );
      if (accepted) updateSubmission({ type: "accepted", id,
        runId, steeringId: accepted.steeringId });
      if (currentScope.current === submissionScope) setFollowRequest((request) => request + 1);
      return { ok: true };
    } catch (error) {
      updateSubmission({ type: "withdrawn", id });
      return { ok: false, error };
    }
  };

  /// The composer's model choice becomes the session default. Config
  /// replacement needs an idle session and the revision last read.
  const saveModelDefault = async (config: Record<string, unknown>) => {
    if (!canConfigure) return;
    const current = session.data;
    if (!current) return;
    const updated = await api<SessionView>(
      "PUT",
      `/api/ui/universes/${universeId}/sessions/${sessionId}/config`,
      { config, expectedConfigRevision: current.configRevision },
    );
    queryClient.setQueryData(["session", universeId, sessionId], updated);
  };

  const cancelRun = async (runId: string) => {
    if (!canStop) return null;
    setSendError(null);
    try {
      return await api<SessionRunCancelled>(
        "POST",
        `/api/ui/universes/${universeId}/sessions/${sessionId}/runs/${runId}/cancel`,
        {},
      );
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
      return null;
    }
  };

  const stop = async () => {
    // Stop the run the engine is executing; if only queued runs exist
    // (the active one just ended), stop the next one instead so the queue
    // does not start behind the reader's back.
    const target =
      steerTargetRunId ?? queuedRuns[0]?.runId ?? resolvedPending.find((m) => m.runId)?.runId;
    if (!target) {
      return;
    }
    if (queueActions.current.has(target)) return;
    if (steerTargetRunId === target) {
      setStoppingRunId(target);
    } else {
      setCancellingQueued((prev) => new Set(prev).add(target));
    }
    const response = await cancelRun(target);
    if (!response) {
      setStoppingRunId((current) => (current === target ? null : current));
      setCancellingQueued((prev) => {
        const next = new Set(prev);
        next.delete(target);
        return next;
      });
      return;
    }
    if (response.run.status === "cancelled") {
      setStoppingRunId((current) => (current === target ? null : current));
    }
    void session.refetch();
  };

  const cancelQueued = async (runId: string) => {
    if (queueActions.current.has(runId)) return;
    queueActions.current.add(runId);
    setCancellingQueued((prev) => new Set(prev).add(runId));
    const response = await cancelRun(runId);
    queueActions.current.delete(runId);
    if (!response) {
      setCancellingQueued((prev) => {
        const next = new Set(prev);
        next.delete(runId);
        return next;
      });
      return;
    }
    if (response.run.status === "cancelled" && response.run.startedAtMs == null) {
      const sent = resolvedPending.find(item => item.runId === runId);
      if (sent) updateSubmission({ type: "withdrawn", id: sent.id });
    }
    void session.refetch();
  };

  const steerQueued = async (runId: string) => {
    const target = steerTargetRunId;
    if (!canSteer || !canStop || closed || (controlledGate && !directInput)
      || !target || target === runId || queueActions.current.size > 0) return;
    queueActions.current.add(runId);
    setSteeringQueued(runId);
    setSendError(null);
    let message: ComposerMessage | undefined;
    let cancellationAttempted = false;
    try {
      const run = await api<RunView>("GET", `/api/ui/universes/${universeId}/sessions/${sessionId}/runs/${runId}`);
      if (run.status !== "queued") throw new Error("This message is no longer queued.");
      message = await queuedMessage(run, loadFullText);
      if (currentScope.current !== submissionScope) return;
      if (queueSteerTarget.current !== target) {
        throw new Error("The active run is no longer available for steering. The message is still queued.");
      }
      storeQueuedRecovery(submissionScope, runId, {
        id: runId, scope: submissionScope, message,
        error: "Conversion was interrupted. Check the queue and conversation before sending this saved message again.",
      });
      cancellationAttempted = true;
      const cancelled = await api<SessionRunCancelled>(
        "POST", `/api/ui/universes/${universeId}/sessions/${sessionId}/runs/${runId}/cancel`, {},
      );
      if (cancelled.run.status !== "cancelled" || cancelled.run.startedAtMs != null) {
        throw new Error("The queued message started before it could be converted. Steering was not sent.");
      }
      if (currentScope.current === submissionScope) {
        const sent = resolvedPending.find(item => item.runId === runId);
        if (sent) updateSubmission({ type: "withdrawn", id: sent.id });
        reconcileRuns([{ id: runId, status: "cancelled", acceptedAtMs: 0, source: { type: "input" } }]);
      }
      // Keep the original target even if the tail reports a different active run.
      const result = await steer(message, target);
      if (result.ok) {
        dismissRecoveredMessage(submissionScope, runId);
      } else if (steeringWasNotSubmitted(result.error)) {
        const recovery = {
          id: runId, scope: submissionScope, message,
          error: "Steering was not sent. The follow-up could not be confirmed. Use Queue again to retry.",
        };
        storeQueuedRecovery(submissionScope, runId, recovery);
        if (await send(message, "queue", queuedRecoverySubmissionId(runId))) {
          dismissRecoveredMessage(submissionScope, runId);
          if (currentScope.current === submissionScope) setNotices((prev) => [...prev, {
            id: queuedRecoverySubmissionId(runId),
            text: "Steering was unavailable. Sent your message as a follow-up.",
          }]);
        } else {
          recoverQueuedMessage(recovery);
        }
      } else {
        if (currentScope.current === submissionScope) {
          setSendError(result.error instanceof Error ? result.error.message : String(result.error));
        }
        recoverQueuedMessage({
          id: runId, scope: submissionScope, message,
          error: "Removed from the queue, but steering could not be confirmed. Check the conversation before sending again.",
        });
      }
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      if (currentScope.current === submissionScope) setSendError(detail);
      if (message && cancellationAttempted) {
        recoverQueuedMessage({
          id: runId, scope: submissionScope, message,
          error: `${detail} Steering was not sent. Check the queue and conversation before sending again.`,
        });
      }
    } finally {
      queueActions.current.delete(runId);
      setSteeringQueued((current) => current === runId ? null : current);
      void session.refetch();
    }
  };

  const recoverQueuedMessage = (recovery: QueuedMessageRecovery) => {
    storeQueuedRecovery(recovery.scope, recovery.id, recovery);
    setRecoveredMessages((prev) => [
      ...prev.filter((item) => item.scope !== recovery.scope || item.id !== recovery.id), recovery,
    ]);
  };

  const dismissRecoveredMessage = (scope: string, id: string) => {
    storeQueuedRecovery(scope, id);
    setRecoveredMessages((prev) => prev.filter((item) => item.scope !== scope || item.id !== id));
  };

  const requeueRecovered = async (id: string, message: ComposerMessage) => {
    if (queueActions.current.has(id)) return;
    queueActions.current.add(id);
    setRecoveredMessages((prev) => prev.map((item) => item.id === id ? { ...item, sending: true } : item));
    const sent = await send(message, "queue", queuedRecoverySubmissionId(id));
    if (sent) dismissRecoveredMessage(submissionScope, id);
    else setRecoveredMessages((prev) => prev.map((item) => item.id === id ? { ...item, sending: false } : item));
    queueActions.current.delete(id);
  };

  const decideApproval = async (
    approvalId: string,
    decision: "approve" | "reject",
  ) => {
    if (!canControl || !approvalRun) return;
    setApprovalError(null);
    setDecidingApproval({ approvalId, decision });
    try {
      const response = await api<SessionRunApprovalsDecided>(
        "POST",
        `/api/ui/universes/${universeId}/sessions/${sessionId}/runs/${approvalRun.id}/approvals`,
        { decisions: [{ approvalId, decision }] },
      );
      const result = response.results[0];
      if (!result || result.status === "failed") {
        throw new Error(result?.failure?.message ?? "The approval decision was not accepted");
      }
      if (result.status === "pending") {
        setApprovalError({ approvalId, message: "The session is busy, so this decision has not applied yet. It can still apply." });
      }
      queryClient.setQueryData<SessionView>(
        ["session", universeId, sessionId],
        (current) => current
          ? {
              ...current,
              runs: current.runs?.map((run) =>
                run.id === response.run.id ? response.run : run,
              ),
            }
          : current,
      );
      await session.refetch();
    } catch (error) {
      setApprovalError({
        approvalId,
        message: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setDecidingApproval(null);
    }
  };

  const closeSession = useMutation({
    meta: { blocking: true },
    mutationFn: () =>
      api<SessionView>(
        "POST",
        `/api/ui/universes/${universeId}/sessions/${sessionId}/close`,
        { force: true },
      ),
    onSuccess: async (closedSession) => {
      setCloseOpen(false);
      queryClient.setQueryData(
        ["session", universeId, sessionId],
        closedSession,
      );
      await queryClient.invalidateQueries(sessionCollections(universeId));
    },
    onError: (error) => setCloseError(error.message),
  });

  const restartSession = useMutation({
    meta: { blocking: true },
    mutationFn: () =>
      api<SessionView>(
        "POST",
        `/api/ui/universes/${universeId}/sessions/${sessionId}/restart`,
        {},
      ),
    onSuccess: async (restarted) => {
      queryClient.setQueryData(["session", universeId, sessionId], restarted);
      await queryClient.invalidateQueries(sessionCollections(universeId));
    },
  });

  const onSessionDeleted = async () => {
    setDeleteOpen(false);
    queryClient.setQueriesData<InfiniteData<SessionListPage>>(
      sessionCollections(universeId),
      (current) => current
        ? {
            ...current,
            pages: current.pages.map((page) => ({
              ...page,
              sessions: page.sessions.filter((candidate) => candidate.id !== sessionId),
            })),
          }
        : current,
    );
    navigate(backTo);
    await queryClient.invalidateQueries(sessionCollections(universeId));
  };

  return (
    <>
      <ProviderReadinessBanner universeId={universeId} handle={handle} model={modelFromConfig(session.data?.config)} enabled={Boolean(session.data)} />
      {session.data?.executionFailure && (
        <div role="alert" className="flex items-center gap-3 border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          <span className="min-w-0 flex-1">
            {EXECUTION_FAILURE_TEXT[session.data.executionFailure]} Its history is intact, but nothing is running and new messages will fail.
            {restartSession.error && ` Restart failed: ${restartSession.error.message}`}
          </span>
          {canConfigure && (
            <Button
              size="sm"
              variant="outline"
              className="shrink-0"
              disabled={restartSession.isPending}
              onClick={() => restartSession.mutate()}
            >
              {restartSession.isPending ? "Restarting…" : "Restart"}
            </Button>
          )}
        </div>
      )}
      {embedded && embeddedHeader && (
        // As the page's header, for a session inside another page: what is
        // open, its state and what it can reach, then its ⋯.
        <DetailHeader
          title={embeddedHeader.title}
          titleClassName="overflow-visible"
          tags={meta.length > 0 && (
            <MetaToolbar
              items={meta.filter((item) => item.key !== SESSION_META.controller)}
              place="header"
              full
              label="Conversation details"
              className="grow"
            />
          )}
          tools={embeddedHeader.actions ? [{ kind: "custom", key: "conversation", node: embeddedHeader.actions }] : []}
        />
      )}
      {!embedded && (
        <>
        {/* Left is state: what it is, who sees it, who drives it, what it is
            doing. Right is action, all of it behind ⋯. */}
        <DetailHeader
          back={{ to: backTo, label: "Back to sessions" }}
          title={title}
          tags={meta.length > 0 && <MetaToolbar items={meta} place="header" full label="Session details" className="grow" />}
          tools={[
            // A tool for those who can change the settings; readers open them
            // read-only from the ⋯ menu.
            ...(canConfigure
              ? [{ key: "settings", label: "Session settings", icon: <ConfigureIcon />, onSelect: () => setSettingsOpen(true) }]
              : []),
            { kind: "custom",
            key: "session",
            node: <SessionActionsMenu
              sessionId={sessionId}
              metadata={session.data?.metadata}
              open={controller.open}
              onSettings={permissions.can("read") ? () => setSettingsOpen(true) : undefined}
              onCompact={canControl && session.data && !closed ? compaction.compact : undefined}
              compactionLabel={compaction.label}
              onShare={canShare ? () => setShareOpen(true) : undefined}
              lifecycle={((!controlled || permissions.can("administer")) && canClose && !closed) || (canDelete && closed) ? (
                closed ? (
                  <DropdownMenuItem
                    variant="destructive"
                    onClick={() => setDeleteOpen(true)}
                  >
                    <DeleteIcon /> Delete session…
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem
                    variant="destructive"
                    onClick={() => {
                      setCloseError(null);
                      setCloseOpen(true);
                    }}
                  >
                    <SessionCloseIcon /> {runActive ? "Force close session…" : "Close session…"}
                  </DropdownMenuItem>
                )
              ) : undefined}
            />,
          }]}
        />

        <ShareSessionDialog
          universeId={universeId}
          sessionId={sessionId}
          open={shareOpen && canShare}
          onOpenChange={setShareOpen}
        />

        <AlertDialog
          open={closeOpen && canClose}
          onOpenChange={(open) => {
            setCloseOpen(open);
            if (open) setCloseError(null);
          }}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>
                {runActive ? "Force close this session?" : "Close this session?"}
              </AlertDialogTitle>
              <AlertDialogDescription>
                {runActive
                  ? "This cancels active and queued work, then permanently closes the session. Recovery of a stuck workflow can take up to about 90 seconds while the engine terminates it and reconciles the session. The history remains available, but the session cannot be reopened."
                  : "This permanently closes the session. It remains in the session list so its history can be inspected, but it cannot be reopened."}
              </AlertDialogDescription>
            </AlertDialogHeader>
            {closeSession.isPending && (
              <p className="text-sm text-muted-foreground">
                Force close is running in the background. You can hide this dialog and
                continue using the app.
              </p>
            )}
            {closeError && <p className="text-sm text-destructive">{closeError}</p>}
            <AlertDialogFooter>
              <AlertDialogCancel>
                {closeSession.isPending ? "Hide" : "Cancel"}
              </AlertDialogCancel>
              <AlertDialogAction
                className="bg-destructive text-white hover:bg-destructive/90"
                disabled={closeSession.isPending}
                onClick={() => closeSession.mutate()}
              >
                {closeSession.isPending
                  ? "Force-closing…"
                  : runActive
                    ? "Force close session"
                    : "Close session"}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        {deleteOpen && <DeleteSessionDialog
          key={`${universeId}:${sessionId}`}
          universeId={universeId}
          sessionId={sessionId}
          onCancel={() => setDeleteOpen(false)}
          onDeleted={onSessionDeleted}
        />}
        </>
      )}
      {showStrip && (
        <div className="shrink-0 border-b bg-muted/30">
          <div className="mx-auto w-full max-w-5xl px-4 py-1.5 md:px-8">
            {/* Embedded in its bot's page, a session need not name the bot. */}
            <MetaToolbar
              items={embedded ? meta.filter((item) => item.key !== SESSION_META.controller) : meta}
              place="strip"
              label="Session details"
            />
          </div>
        </div>
      )}
      <TranscriptLinksContext.Provider value={transcriptLinks}>
      <MessageScrollerProvider key={`${universeId}/${sessionId}`} autoScroll defaultScrollPosition="end">
        <TranscriptMotionProvider sections={displaySections} ready={tail.phase === "live"} historyRevision={tail.historyRevision}>
        <MessageScroller className="min-h-0 flex-1">
          <MessageScrollerViewport preserveScrollOnPrepend>
            <SessionHistoryLoader tail={tail} />
            <MessageScrollerContent className="mx-auto w-full max-w-5xl cursor-select gap-3 px-4 py-6 md:px-8">
              {tail.phase === "loading" && entries.length === 0 && !tail.error && (
                <LoadingNote />
              )}
              {tail.error && entries.length === 0 && (
                <ReadError error={tail.error} transient={tail.errorIsTransient} retrying loading graceMs={10_000} />
              )}
              {tail.phase === "live" &&
                entries.length === 0 &&
                pendingInTranscript.length === 0 && (
                  <SessionWelcome
                    state={closed ? "closed" : !canControl ? "readOnly" : controlledGate && !directInput ? "controlled" : "open"}
                    unshared={owner && session.data?.access.visibility === "restricted" && !session.data?.origin}
                  />
                )}
              {displaySections.map((section) => (
                <MessageScrollerItem key={section.key} messageId={section.key}>
                  {section.kind === "run" ? (
                    <RunSectionView
                      section={section}
                      activeRun={activeRun}
                      loadFullText={loadFullText}
                      showRunStatistics={showRunStatistics}
                      collapseCompletedRuns={collapseCompletedRuns}
                    />
                  ) : section.kind === "system" ? (
                    <SystemChips entries={section.entries} />
                  ) : (
                    <TranscriptEntryView entry={section.entry} loadFullText={loadFullText} />
                  )}
                </MessageScrollerItem>
              ))}
              {[...notices, ...submissionView.notices].map((notice) => (
                <MessageScrollerItem key={notice.id} messageId={notice.id}>
                  <TranscriptEntryView
                    entry={{ kind: "marker", key: notice.id, text: notice.text, tone: "muted" }}
                  />
                </MessageScrollerItem>
              ))}
              {compaction.error && (
                <MessageScrollerItem messageId="compaction-request-error">
                  <TranscriptEntryView entry={{
                    kind: "marker", key: "compaction-request-error",
                    text: `Compaction failed: ${compaction.error}`, tone: "error",
                  }} />
                </MessageScrollerItem>
              )}
              {approvalRun && (
                <MessageScrollerItem messageId={`approvals-${approvalRun.id}`}>
                  <ApprovalCards
                    approvals={approvalRun.pendingApprovals ?? []}
                    deciding={decidingApproval}
                    error={approvalError}
                    onDecide={canControl ? (approvalId, decision) =>
                      void decideApproval(approvalId, decision) : undefined}
                  />
                </MessageScrollerItem>
              )}
              {tail.error && entries.length > 0 && (
                <ReadError error={tail.error} transient={tail.errorIsTransient} retrying graceMs={10_000} className="text-center text-xs" />
              )}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
        <SessionScrollFollower
          followRequest={followRequest}
          ready={tail.phase === "live"}
          historyRevision={tail.historyRevision}
          entries={entries}
          pending={pendingInTranscript}
          activeRun={activeRun}
        />
        </TranscriptMotionProvider>
      </MessageScrollerProvider>
      </TranscriptLinksContext.Provider>
      {!closed && (
        <QueuedRunsBar
          items={queuedItems}
          onCancel={canStop ? (runId) => void cancelQueued(runId) : undefined}
          onSteer={canControl && canStop && (!controlledGate || directInput) ? (runId) => void steerQueued(runId) : undefined}
          canSteer={canSteer && steeringQueued === null}
        />
      )}
      {recoveredMessages.filter((item) => item.scope === submissionScope).map((item) => (
        <div key={item.id} className="shrink-0 border-t bg-muted/40 px-4 py-2" role="alert">
          <div className="mx-auto flex w-full max-w-5xl flex-col gap-2 text-sm md:px-4">
            <p>{item.error}</p>
            <details>
              <summary className="cursor-action text-xs">Saved message</summary>
              <p className="max-h-40 overflow-auto whitespace-pre-wrap">{item.message.text}</p>
              {item.message.attachments.map((attachment) => <p key={attachment.blobRef}>{attachment.name}</p>)}
            </details>
            <div className="flex gap-2">
              <Button size="xs" variant="outline"
                disabled={item.sending || !canControl || closed || (controlledGate && !directInput)}
                onClick={() => void requeueRecovered(item.id, item.message)}>Queue again</Button>
              <Button size="xs" variant="ghost" disabled={item.sending}
                onClick={() => dismissRecoveredMessage(item.scope, item.id)}>Dismiss</Button>
            </div>
          </div>
        </div>
      ))}
      <SessionComposer
        dictation={{ ...dictation, settingsHref: `/u/${handle}/models` }}
        key={sessionDraftKey(universeId, sessionId)}
        draftKey={sessionDraftKey(universeId, sessionId)}
        runActive={runActive}
        canSteer={canSteer}
        stopping={stopping}
        canStop={canStop && !closed}
        disabled={!canControl || closed || (controlledGate && !directInput)}
        disabledReason={!canControl
          ? "You have read-only access to this session."
          : controlledGate && !directInput
          ? `Managed by ${managerLabel} — flip Direct input to message this session anyway.`
          : undefined}
        banner={canControl && controlledGate && !closed ? (
          <div className="flex min-w-0 items-center gap-2 pb-2 text-xs">
            <Switch
              className="shrink-0"
              checked={directInput}
              onCheckedChange={setDirectInput}
              aria-label="Direct input"
            />
            <span className="shrink-0 font-medium">Direct input</span>
            <span
              className={`min-w-0 truncate ${directInput ? "text-foreground" : "text-muted-foreground"}`}
              title={directInput
                ? `Direct input bypasses ${managerLabel}'s ingress: messages are not tracked as events, skip its budget and delivery policies, and may interleave with its deliveries.`
                : `Managed by ${managerLabel} — flip Direct input to message this session anyway.`}
            >
              {directInput
                ? `Bypasses ${managerLabel}'s ingress: messages are not tracked as events, skip its budget and delivery policies, and may interleave with its deliveries.`
                : `Managed by ${managerLabel} — flip to message this session anyway.`}
            </span>
          </div>
        ) : undefined}
        attachments={{ universeId, apiKind: modelFromConfig(session.data?.config)?.apiKind }}
        model={canConfigure && session.data?.config ? {
          config: session.data.config,
          models: modelDiscovery.data?.models,
          canSaveDefault: canConfigure && !closed,
          onSaveDefault: saveModelDefault,
        } : undefined}
        error={sendError}
        onDismissError={() => setSendError(null)}
        onSend={(message, mode) => void send(message, mode)}
        onStop={() => void stop()}
      />
      {!embedded && permissions.can("read") && (
        <SessionSettingsDialog
          controllerLabel={controller.label}
          universeId={universeId}
          sessionId={sessionId}
          session={session.data}
          runActive={runActive}
          open={settingsOpen}
          onOpenChange={(open) => {
            setSettingsOpen(open);
            if (open) void session.refetch();
          }}
        />
      )}
    </>
  );
}

/// The attachment fields the message routes accept.
function wireAttachment({ blobRef, mime, kind, name }: SentAttachment) {
  return { blobRef, mime, kind, name };
}

/// Attachments as transcript media for the optimistic echo, previewed from
/// the picked files until the stored entries arrive.
function echoMedia(attachments: SentAttachment[]): TranscriptMedia[] | undefined {
  if (!attachments.length) return undefined;
  return attachments.map((attachment) => ({
    handle: mediaHandleFor(attachment.blobRef),
    blobRef: attachment.blobRef,
    mime: attachment.mime,
    kind: attachment.kind,
    name: attachment.name,
    ...(attachment.previewUrl ? { localUrl: attachment.previewUrl } : {}),
  }));
}

function attachmentSummary(media: TranscriptMedia[] | undefined): string {
  const count = media?.length ?? 0;
  if (count === 1) return media![0]!.name ?? "1 attachment";
  return count ? `${count} attachments` : "";
}

/// Text for a queued run: from the authoritative session view when it has
/// been refetched, else from the optimistic send that produced it.
function queuedRunText(
  runId: string,
  runs: SessionRunView[] | undefined,
  pending: LocalSubmission[],
): string {
  const run = runs?.find((candidate) => String(candidate.id) === runId);
  if (run?.source.type === "input") {
    const text = run.source.preview?.trim();
    if (text) {
      return text;
    }
  }
  const sent = pending.find((message) => message.runId === runId);
  return sent?.text || attachmentSummary(sent?.media) || "(queued message)";
}

function SessionScrollFollower({
  followRequest,
  ready,
  entries,
  pending,
  activeRun,
  historyRevision,
}: {
  followRequest: number;
  ready: boolean;
  entries: TranscriptEntry[];
  pending: { id: string; text: string }[];
  activeRun: ActiveRun | null;
  historyRevision: number;
}) {
  const { scrollToEnd } = useMessageScroller();
  const scrollable = useMessageScrollerScrollable();
  const initialized = useRef(false);
  const followedRequest = useRef(0);
  const followedHistory = useRef(historyRevision);

  useLayoutEffect(() => {
    if (followRequest !== followedRequest.current) {
      followedRequest.current = followRequest;
      // With autoScroll enabled, scrollToEnd also switches the scroller
      // from manual reading back to following-bottom, including during catch-up.
      scrollToEnd({ behavior: "auto" });
    }
    if (!ready) {
      return;
    }
    if (followedHistory.current !== historyRevision) {
      followedHistory.current = historyRevision;
      return;
    }

    // On open, always start at the latest message. After that, append only
    // follows when the viewport was already at its end before this render;
    // a reader who scrolled up keeps their position.
    if (!initialized.current || !scrollable.end) {
      initialized.current = true;
      scrollToEnd({ behavior: "auto" });
    }
  }, [followRequest, ready, entries, pending, activeRun, historyRevision, scrollable.end, scrollToEnd]);

  return null;
}

/// An empty session: an invitation to whoever can write to it, a plain note
/// to anyone who cannot.
function SessionWelcome({
  state,
  unshared,
}: {
  state: "open" | "closed" | "readOnly" | "controlled";
  unshared: boolean;
}) {
  const [title, body] = {
    open: [
      "What should we work on?",
      "Ask a question or hand over a task. The agent works through it here, step by step, and you can steer it while it runs.",
    ],
    controlled: [
      "Nothing here yet",
      "This session is driven by its manager; its work will appear here.",
    ],
    readOnly: ["Nothing here yet", "Messages and the agent's work will appear here."],
    closed: ["This session is closed", "It ended before anything was said."],
  }[state];
  return (
    <div className="flex min-h-[45vh] flex-col items-center justify-center gap-3 px-4 text-center">
      <SessionIcon className="size-10 text-muted-foreground/50" />
      <h2 className="text-lg font-medium tracking-tight">{title}</h2>
      <p className="max-w-md text-sm text-muted-foreground">{body}</p>
      {unshared && state === "open" && (
        <p className="max-w-md text-xs text-muted-foreground">
          Only you and the universe's admins see this session until you share it.
        </p>
      )}
    </div>
  );
}

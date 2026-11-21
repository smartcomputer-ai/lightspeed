import { createContext, type ComponentType, type ReactNode } from "react";
import type { SessionView } from "@/api";
import type { SessionMetaInput } from "@session-ui/components/session-meta";

/** Optional enrichment of a managed session, supplied by its controller's UI. */
export interface SessionControllerPresentation {
  label: string;
  attribution?: SessionMetaInput["controller"];
  open?: { label: string; href: string; icon: ReactNode };
  participantName?: (id: string) => string | undefined;
}

export interface ControllerPresentationProps {
  universeId: string;
  handle: string;
  session: Pick<SessionView, "id" | "metadata" | "integration"> | undefined;
  children: (presentation: SessionControllerPresentation) => ReactNode;
}

export function PlainSessionController({ session, children }: ControllerPresentationProps) {
  return children({ label: session?.integration?.lifecycleController?.workflowKind || "an external workflow" });
}

export const SessionControllerPresenter = createContext<ComponentType<ControllerPresentationProps>>(PlainSessionController);

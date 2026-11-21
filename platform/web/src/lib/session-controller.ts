import { createContext, useContext, type ComponentType, type ReactNode } from "react";
import type { SessionView } from "@/api";
import type { IconComponent } from "@/icons";

/** Optional enrichment of a managed session, supplied by its controller's UI. */
export interface SessionControllerPresentation {
  label: string;
  attribution?: { label: string; icon?: IconComponent; iconColor?: string; href?: string };
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

/** Reads the presenter selected by the host, with a plain workflow label as the default. */
export function useSessionControllerPresenter() {
  return useContext(SessionControllerPresenter);
}

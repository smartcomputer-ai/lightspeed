import { createContext, useContext, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { TranscriptSection } from "@/lib/sessions/run-sections";
import { cn } from "@/lib/utils";

const MotionContext = createContext<(key: string) => boolean>(() => false);

/// Remember arrivals, including hidden work, so history and remounted replies
/// never replay their entrance. The first live snapshot is the baseline.
export function TranscriptMotionProvider({ sections, pendingKeys, ready, historyRevision, children }: {
  sections: TranscriptSection[];
  pendingKeys: string[];
  ready: boolean;
  historyRevision: number;
  children: ReactNode;
}) {
  const previous = useRef({ ready: false, historyRevision, seen: new Set<string>() });
  const animateNew = ready && previous.current.ready && previous.current.historyRevision === historyRevision;
  const canAnimate = (key: string) => animateNew && !previous.current.seen.has(key);

  useLayoutEffect(() => {
    const seen = previous.current.seen;
    for (const section of sections) {
      if (section.kind === "run") {
        if (section.input) seen.add(section.input.key);
        if (section.reply) seen.add(section.reply.key);
        for (const entry of section.work) seen.add(entry.key);
        if (section.live || section.work.length > 0) seen.add(`activity:${section.key}`);
      } else if (section.kind === "entry") {
        seen.add(section.entry.key);
      }
    }
    for (const key of pendingKeys) seen.add(key);
    previous.current.ready = ready;
    previous.current.historyRevision = historyRevision;
  }, [sections, pendingKeys, ready, historyRevision]);

  return <MotionContext.Provider value={canAnimate}>{children}</MotionContext.Provider>;
}

export function TranscriptEntrance({ motionKey, className, children }: {
  motionKey: string;
  className?: string;
  children: ReactNode;
}) {
  const canAnimate = useContext(MotionContext);
  const [entering, setEntering] = useState(() => canAnimate(motionKey));
  const element = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const node = element.current;
    if (!entering || !node) return;
    const finish = (event: AnimationEvent) => {
      if (event.target === node && event.animationName === "transcript-enter") setEntering(false);
    };
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    const checkMotion = () => {
      if (reducedMotion?.matches) setEntering(false);
    };
    node.addEventListener("animationend", finish);
    node.addEventListener("animationcancel", finish);
    reducedMotion?.addEventListener("change", checkMotion);
    checkMotion();
    return () => {
      node.removeEventListener("animationend", finish);
      node.removeEventListener("animationcancel", finish);
      reducedMotion?.removeEventListener("change", checkMotion);
    };
  }, [entering]);
  // When a whole activity box arrives, its contents move with it. Later
  // messages can enter individually only after its animation has finished.
  return (
    <MotionContext.Provider value={(key) => !entering && canAnimate(key)}>
      <div ref={element} className={cn("min-w-0", className, entering && "transcript-enter")}>{children}</div>
    </MotionContext.Provider>
  );
}

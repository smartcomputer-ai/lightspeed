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
  const [animate] = useState(() => canAnimate(motionKey));
  // When a whole activity box arrives, its contents move with it. Later
  // messages can enter individually after the box has been committed.
  return (
    <MotionContext.Provider value={(key) => !canAnimate(motionKey) && canAnimate(key)}>
      <div className={cn("min-w-0", className, animate && "transcript-enter")}>{children}</div>
    </MotionContext.Provider>
  );
}

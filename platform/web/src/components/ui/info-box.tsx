import type { ComponentProps, ReactNode } from "react";
import { FailedIcon, InfoIcon, SuccessIcon, WarningIcon, type IconComponent } from "@/icons";
import { cn } from "@/lib/utils";

/// A note in a box: info, success, warning or error, each in its status
/// color with its icon, an optional title beside the icon and the text
/// under it. The colors are --note-<tone>-background, -border and -title
/// in index.css (editable in the design configurator).

export type InfoBoxTone = "info" | "success" | "warning" | "error";

const TONES: Record<InfoBoxTone, { icon: IconComponent; box: string; title: string }> = {
  info: { icon: InfoIcon, box: "border-(--note-info-border) bg-(--note-info-background)", title: "text-(--note-info-title)" },
  success: { icon: SuccessIcon, box: "border-(--note-success-border) bg-(--note-success-background)", title: "text-(--note-success-title)" },
  warning: { icon: WarningIcon, box: "border-(--note-warning-border) bg-(--note-warning-background)", title: "text-(--note-warning-title)" },
  error: { icon: FailedIcon, box: "border-(--note-error-border) bg-(--note-error-background)", title: "text-(--note-error-title)" },
};

export function InfoBox({ tone = "info", title, className, children, ...props }: Omit<ComponentProps<"div">, "title"> & {
  tone?: InfoBoxTone;
  title?: ReactNode;
}) {
  const { icon: Icon, box, title: titleColor } = TONES[tone];
  return (
    <div
      // An error is announced; the others are notes beside the content.
      role={tone === "error" ? "alert" : "note"}
      data-slot="info-box"
      data-tone={tone}
      className={cn("grid grid-cols-[auto_minmax(0,1fr)] items-start gap-x-2 gap-y-1 rounded-lg border px-3 py-2.5 text-xs leading-snug", box, className)}
      {...props}
    >
      {/* As tall as the first line (title or text) and centred in it, which
          puts it between the capitals' and the lowercase letters' middles,
          where mixed text reads as centred. */}
      <span className="flex h-lh items-center" aria-hidden>
        <Icon className={cn("size-3.5 shrink-0", titleColor)} />
      </span>
      {title !== undefined && title !== null && <p className={cn("font-semibold", titleColor)}>{title}</p>}
      {children !== undefined && children !== null && (
        <div className={cn("text-foreground/80", title !== undefined && title !== null && "col-start-2")}>{children}</div>
      )}
    </div>
  );
}

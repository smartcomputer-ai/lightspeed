import { forwardRef, type SVGProps } from "react";

/// Lightspeed bot mark: a round little bot leaning into its own speed,
/// motion streaks trailing behind. The whole glyph paints with
/// `currentColor` and the eyes are punched out of the body, so one drawing
/// serves every context: in the sidebar it inherits the text color and
/// reads monochrome; on cards and avatars set `color` per bot.
type BotMarkProps = SVGProps<SVGSVGElement> & { size?: number | string };

/// Outlined rendition on the lucide 24px grid (stroke 2, round caps) so it
/// sits flush with lucide icons in the sidebar: `<NavItem icon={BotIcon} />`.
/// Same anatomy as the mark — fanned streaks, eyes tilted by the same 7°.
export const BotIcon = forwardRef<SVGSVGElement, BotMarkProps>(
  ({ size = 24, ...props }, ref) => (
    <svg
      ref={ref}
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      <circle cx={14.5} cy={12} r={7} />
      <g transform="rotate(7 14.5 12)">
        <path d="M12.4 10.2v1.5" />
        <path d="M16.6 10.2v1.5" />
      </g>
      <path d="M3 7.5h2.5" />
      <path d="M1.5 12h3" />
      <path d="M3 16.5h2.5" />
    </svg>
  ),
);
BotIcon.displayName = "BotIcon";

/// Outlined face without streaks, centered on the lucide grid. The body is
/// drawn 15% larger than BotIcon's (streaks need no room), so it reads at
/// the same optical size as neighboring lucide icons.
export const BotFaceIcon = forwardRef<SVGSVGElement, BotMarkProps>(
  ({ size = 24, ...props }, ref) => (
    <svg
      ref={ref}
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      <circle cx={12} cy={12} r={8} />
      <g transform="rotate(7 12 12)">
        <path d="M9.6 9.95v1.7" />
        <path d="M14.4 9.95v1.7" />
      </g>
    </svg>
  ),
);
BotFaceIcon.displayName = "BotFaceIcon";

export const BotMark = forwardRef<SVGSVGElement, BotMarkProps>(
  ({ size = 24, ...props }, ref) => (
    <svg
      ref={ref}
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 48 48"
      fill="none"
      aria-hidden="true"
      {...props}
    >
      <g stroke="currentColor" strokeWidth={3.5} strokeLinecap="round">
        <path d="M6 15h6.5" />
        <path d="M2 25h7" />
        <path d="M6.5 35h6" />
      </g>
      <path
        transform="rotate(7 28 25)"
        fill="currentColor"
        fillRule="evenodd"
        d="M12 25a16 16 0 1 0 32 0 16 16 0 1 0-32 0ZM21.4 19.2a2.2 2.2 0 0 1 4.4 0v4.6a2.2 2.2 0 0 1-4.4 0ZM30.2 19.2a2.2 2.2 0 0 1 4.4 0v4.6a2.2 2.2 0 0 1-4.4 0Z"
      />
    </svg>
  ),
);
BotMark.displayName = "BotMark";

/// The bot without his speed streaks — same lean, same punched-out eyes,
/// centered in the viewBox. For tight spots where the trailing lines crowd
/// (favicon, tiny avatars, badges).
export const BotFace = forwardRef<SVGSVGElement, BotMarkProps>(
  ({ size = 24, ...props }, ref) => (
    <svg
      ref={ref}
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      viewBox="0 0 48 48"
      fill="none"
      aria-hidden="true"
      {...props}
    >
      <path
        transform="rotate(7 24 24)"
        fill="currentColor"
        fillRule="evenodd"
        d="M8 24a16 16 0 1 0 32 0 16 16 0 1 0-32 0ZM17.4 18.2a2.2 2.2 0 0 1 4.4 0v4.6a2.2 2.2 0 0 1-4.4 0ZM26.2 18.2a2.2 2.2 0 0 1 4.4 0v4.6a2.2 2.2 0 0 1-4.4 0Z"
      />
    </svg>
  ),
);
BotFace.displayName = "BotFace";

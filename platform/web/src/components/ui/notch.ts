/// The notch of a popover or tooltip: a small square in the popover surface,
/// turned 45° and centred on the popup's edge, so half of it sticks out as
/// a point towards the element that opened it. Its two outer edges carry
/// the surface border, its inner half melts into the popup. Base UI places
/// it along the edge (over the trigger's centre) and sets `data-side`.
export const NOTCH_CLASS = [
  "pointer-events-none size-3 rotate-45 rounded-[2px] bg-(--surface-popover)",
  "border-0 border-(--surface-popover-border)",
  // Popup below its trigger: the notch on its top edge, pointing up.
  "data-[side=bottom]:-top-1.5 data-[side=bottom]:border-t-(length:--surface-popover-border-width) data-[side=bottom]:border-l-(length:--surface-popover-border-width)",
  // Above: on its bottom edge, pointing down.
  "data-[side=top]:-bottom-1.5 data-[side=top]:border-b-(length:--surface-popover-border-width) data-[side=top]:border-r-(length:--surface-popover-border-width)",
  // To the left of its trigger: on its right edge, pointing right.
  "data-[side=left]:-right-1.5 data-[side=left]:border-t-(length:--surface-popover-border-width) data-[side=left]:border-r-(length:--surface-popover-border-width)",
  "data-[side=inline-start]:-right-1.5 data-[side=inline-start]:border-t-(length:--surface-popover-border-width) data-[side=inline-start]:border-r-(length:--surface-popover-border-width)",
  // To the right: on its left edge, pointing left.
  "data-[side=right]:-left-1.5 data-[side=right]:border-b-(length:--surface-popover-border-width) data-[side=right]:border-l-(length:--surface-popover-border-width)",
  "data-[side=inline-end]:-left-1.5 data-[side=inline-end]:border-b-(length:--surface-popover-border-width) data-[side=inline-end]:border-l-(length:--surface-popover-border-width)",
].join(" ");

/// How far a notched popup sits from its trigger: room for the notch's point.
export const NOTCH_OFFSET = 10;

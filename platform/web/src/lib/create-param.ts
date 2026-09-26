import { useSearchParams } from "react-router-dom";

/**
 * A page's create dialog, open while `?new=<kind>` is in the URL, so a
 * header button, an empty pane or a link can all open it. Closing replaces
 * the history entry; opening pushes one, so Back closes the dialog.
 */
export function useCreateParam(kind: string): [boolean, (open: boolean) => void] {
  const [searchParams, setSearchParams] = useSearchParams();
  const open = searchParams.get("new") === kind;
  const setOpen = (next: boolean) => {
    const params = new URLSearchParams(searchParams);
    if (next) params.set("new", kind);
    else params.delete("new");
    setSearchParams(params, { replace: !next });
  };
  return [open, setOpen];
}

import { Button } from "@/components/ui/button";

export function MarkdownViewToggle({ preview, onPreviewChange }: {
  preview: boolean;
  onPreviewChange: (preview: boolean) => void;
}) {
  return (
    <div role="group" aria-label="Markdown view" className="flex shrink-0 items-center gap-1">
      <Button type="button" size="sm" variant={preview ? "ghost" : "secondary"}
        aria-pressed={!preview} onClick={() => onPreviewChange(false)}>
        Source
      </Button>
      <Button type="button" size="sm" variant={preview ? "secondary" : "ghost"}
        aria-pressed={preview} title="Preview Markdown" onClick={() => onPreviewChange(true)}>
        Preview
      </Button>
    </div>
  );
}

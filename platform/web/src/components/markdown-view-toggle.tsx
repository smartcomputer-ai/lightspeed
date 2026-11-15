import { PreviewIcon, SourceViewIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";

/// Source or Preview for a Markdown file: a toggle group.
export function MarkdownViewToggle({ preview, onPreviewChange }: {
  preview: boolean;
  onPreviewChange: (preview: boolean) => void;
}) {
  return (
    <ButtonGroup aria-label="Markdown view">
      <Button size="sm" aria-pressed={!preview} onClick={() => onPreviewChange(false)}>
        <SourceViewIcon data-icon="inline-start" />
        Source
      </Button>
      <Button size="sm" aria-pressed={preview} title="Preview Markdown" onClick={() => onPreviewChange(true)}>
        <PreviewIcon data-icon="inline-start" />
        Preview
      </Button>
    </ButtonGroup>
  );
}

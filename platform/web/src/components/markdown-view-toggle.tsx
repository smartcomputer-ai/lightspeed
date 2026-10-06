import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";

export function MarkdownViewToggle({ preview, onPreviewChange }: {
  preview: boolean;
  onPreviewChange: (preview: boolean) => void;
}) {
  return (
    <Tabs value={preview ? "preview" : "source"}
      onValueChange={(value) => onPreviewChange(value === "preview")}>
      <TabsList aria-label="Markdown view" activateOnFocus>
        <TabsTrigger value="source">Source</TabsTrigger>
        <TabsTrigger value="preview" title="Preview Markdown">Preview</TabsTrigger>
      </TabsList>
    </Tabs>
  );
}

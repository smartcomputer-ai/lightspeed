import { useEffect, useState } from "react";
import { Download } from "lucide-react";
import { buttonVariants } from "@/components/ui/button";

export function PdfPreview({
  bytes,
  name,
}: {
  bytes: Uint8Array;
  name: string;
}) {
  const [loaded, setLoaded] = useState<{
    bytes: Uint8Array;
    url: string;
  } | null>(null);
  useEffect(() => {
    const url = URL.createObjectURL(
      new Blob([bytes as BlobPart], { type: "application/pdf" }),
    );
    setLoaded({ bytes, url });
    return () => URL.revokeObjectURL(url);
  }, [bytes]);
  const url = loaded?.bytes === bytes ? loaded.url : null;
  if (!url) return null;

  if (navigator.pdfViewerEnabled === true) {
    return (
      <iframe
        src={url}
        title={`PDF preview: ${name}`}
        className="block h-full w-full border-0"
      />
    );
  }
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
      <p className="text-sm text-muted-foreground">
        This browser doesn’t support inline PDF viewing.
      </p>
      <a
        href={url}
        download={name.split("/").at(-1) || "document.pdf"}
        className={buttonVariants({ variant: "outline", size: "sm" })}
      >
        <Download />
        Download PDF
      </a>
    </div>
  );
}

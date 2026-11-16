import { PreviewIcon } from "@/icons";
import { PdfPreview } from "@/components/pdf-preview";
import type { FileModeProps, FileProvider } from "../types";
import { effectiveMediaType } from "../media-types";

/// A PDF begins with "%PDF-".
const SIGNATURE = [0x25, 0x50, 0x44, 0x46, 0x2d];

function PdfMode({ file, content }: FileModeProps) {
  return content.kind === "bytes" ? <PdfPreview bytes={content.bytes} name={file.path} /> : null;
}

/// PDFs: the browser's viewer (or a download where it has none).
export const pdfProvider: FileProvider = {
  id: "pdf",
  label: "PDF",
  match: (file) =>
    effectiveMediaType(file) === "application/pdf" || SIGNATURE.every((byte, index) => file.head[index] === byte)
      ? 100
      : null,
  content: "bytes",
  modes: [{ id: "preview", label: "Preview", icon: PreviewIcon, editable: false, component: PdfMode }],
};

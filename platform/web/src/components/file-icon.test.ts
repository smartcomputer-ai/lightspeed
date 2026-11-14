import { describe, expect, it } from "vitest";
import {
  FileArchiveIcon,
  FileAudioIcon,
  FileCodeIcon,
  FileDocIcon,
  FileIcon,
  FileImageIcon,
  FileIniIcon,
  FileJpgIcon,
  FileMdIcon,
  FilePdfIcon,
  FileTsxIcon,
  FileTxtIcon,
  FileVideoIcon,
  FileXlsIcon,
} from "@/icons";
import { fileIcon } from "./file-icon";

describe("fileIcon", () => {
  it("gives a format with its own icon that icon, whatever the case", () => {
    expect(fileIcon("report.pdf")).toBe(FilePdfIcon);
    expect(fileIcon("Budget.XLSX")).toBe(FileXlsIcon);
    expect(fileIcon("notes.docx")).toBe(FileDocIcon);
    expect(fileIcon("photo.jpeg")).toBe(FileJpgIcon);
    expect(fileIcon("list-pane.tsx")).toBe(FileTsxIcon);
    expect(fileIcon("README.md")).toBe(FileMdIcon);
  });

  it("falls back to the kind for formats without an icon", () => {
    expect(fileIcon("package.json")).toBe(FileCodeIcon);
    expect(fileIcon("compose.yaml")).toBe(FileCodeIcon);
    expect(fileIcon("main.go")).toBe(FileCodeIcon);
    expect(fileIcon("deploy.sh")).toBe(FileCodeIcon);
    expect(fileIcon("banner.webp")).toBe(FileImageIcon);
    expect(fileIcon("call.mp3")).toBe(FileAudioIcon);
    expect(fileIcon("demo.mov")).toBe(FileVideoIcon);
    expect(fileIcon("backup.tar.gz")).toBe(FileArchiveIcon);
  });

  it("knows files by name and dotfiles as settings", () => {
    expect(fileIcon("Dockerfile")).toBe(FileCodeIcon);
    expect(fileIcon("LICENSE")).toBe(FileTxtIcon);
    expect(fileIcon(".gitignore")).toBe(FileIniIcon);
    expect(fileIcon(".env.local")).toBe(FileIniIcon);
    expect(fileIcon(".eslintrc")).toBe(FileIniIcon);
  });

  it("draws anything else as a plain file", () => {
    expect(fileIcon("data.bin")).toBe(FileIcon);
    expect(fileIcon("NOTES")).toBe(FileIcon);
  });
});

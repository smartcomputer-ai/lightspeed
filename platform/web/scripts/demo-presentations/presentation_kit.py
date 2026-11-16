"""Shared helpers for the demo's Presentations workspaces: chart drawing,
the Word narrative assembled from Markdown source notes, the PowerPoint deck,
and the fixture modules the demo seeds from.

Each workspace has its own script (personal_assistant.py,
software_factory.py) and a folder with the workspace's text files (the README
and the Source Notes). See those scripts for how to run them.
"""
import base64
import io
import os
import re
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont
import docx
from docx.shared import Cm, Pt, RGBColor as DocxRGB
from pptx import Presentation
from pptx.dml.color import RGBColor
from pptx.enum.shapes import MSO_SHAPE
from pptx.enum.text import MSO_ANCHOR, PP_ALIGN
from pptx.util import Inches, Pt as PPt

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.normpath(os.path.join(HERE, "..", "..", "src", "demo", "fixtures"))

INK = (24, 24, 38)
MUTED = (110, 112, 128)
GRID = (226, 228, 236)

# ---------------------------------------------------------------------------
# Charts
# ---------------------------------------------------------------------------

W, H = 1600, 900
PAD_L, PAD_R, PAD_T, PAD_B = 140, 60, 170, 120

_FONTS = {
    "regular": "C:/Windows/Fonts/segoeui.ttf",
    "semi": "C:/Windows/Fonts/seguisb.ttf",
    "bold": "C:/Windows/Fonts/segoeuib.ttf",
}

def font(size, weight="regular"):
    """Segoe UI where it is installed (the committed images use it);
    Pillow's default font elsewhere."""
    try:
        return ImageFont.truetype(_FONTS[weight], size)
    except OSError:
        return ImageFont.load_default(size)

def canvas(title, subtitle):
    image = Image.new("RGB", (W, H), "white")
    draw = ImageDraw.Draw(image)
    draw.text((PAD_L - 40, 44), title, font=font(46, "semi"), fill=INK)
    draw.text((PAD_L - 40, 104), subtitle, font=font(26), fill=MUTED)
    return image, draw

def axis(draw, lo, hi, step, fmt, right=W - PAD_R):
    """Horizontal grid lines with labels; returns value → y."""
    plot_h = H - PAD_T - PAD_B
    value = lo
    while value <= hi + 1e-9:
        y = H - PAD_B - (value - lo) / (hi - lo) * plot_h
        draw.line((PAD_L, y, right, y), fill=GRID, width=2)
        label = fmt(value)
        width = draw.textlength(label, font=font(24))
        draw.text((PAD_L - 20 - width, y - 16), label, font=font(24), fill=MUTED)
        value += step
    return lambda v: H - PAD_B - (v - lo) / (hi - lo) * plot_h

def legend(draw, x, y, items):
    for i, (name, color) in enumerate(items):
        ly = y + i * 50
        draw.rounded_rectangle((x, ly + 6, x + 30, ly + 30), radius=6, fill=color)
        draw.text((x + 44, ly), name, font=font(25), fill=INK)

def finish(image, note):
    """The source line at the bottom, then a small palette PNG."""
    ImageDraw.Draw(image).text((PAD_L - 40, H - 52), note, font=font(22), fill=MUTED)
    out = io.BytesIO()
    image.quantize(colors=64, method=Image.Quantize.MEDIANCUT).save(out, "PNG", optimize=True)
    return out.getvalue()

# ---------------------------------------------------------------------------
# Narrative (Word), assembled from the source notes
# ---------------------------------------------------------------------------

def read(folder, path):
    with open(os.path.join(folder, path), encoding="utf-8") as f:
        return f.read()

def _runs(paragraph, text, size=None, color=None, italic=False):
    """**bold** and `code` spans; everything else plain."""
    for part in re.split(r"(\*\*[^*]+\*\*|`[^`]+`)", text):
        if not part:
            continue
        if part.startswith("**"):
            run = paragraph.add_run(part[2:-2])
            run.bold = True
        elif part.startswith("`"):
            run = paragraph.add_run(part[1:-1])
            run.font.name = "Consolas"
        else:
            run = paragraph.add_run(part)
        if size:
            run.font.size = Pt(size)
        if color is not None:
            run.font.color.rgb = color
        run.italic = italic

def _table(document, rows, style):
    cells = [[c.strip() for c in row.strip().strip("|").split("|")] for row in rows if not re.match(r"^\|\s*-", row)]
    table = document.add_table(rows=len(cells), cols=len(cells[0]))
    table.style = style
    for r, row in enumerate(cells):
        for c, value in enumerate(row):
            cell = table.cell(r, c)
            cell.text = ""
            _runs(cell.paragraphs[0], value, size=10)
            if r == 0:
                for run in cell.paragraphs[0].runs:
                    run.bold = True
    document.add_paragraph()

def narrative(folder, sources, *, title, subtitle, note, brand, table_style, properties):
    """The Word document: a title, a note saying what it is, then one
    section per source note (its blockquote replaced by a "From" line)."""
    document = docx.Document()
    styles = document.styles
    styles["Normal"].font.name = "Calibri"
    styles["Normal"].font.size = Pt(11)
    for name, size in (("Heading 1", 18), ("Heading 2", 13)):
        styles[name].font.color.rgb = DocxRGB(*brand)
        styles[name].font.size = Pt(size)
    for key, value in properties.items():
        setattr(document.core_properties, key, value)

    muted = DocxRGB(*MUTED)
    _runs(document.add_paragraph(), title, size=24, color=DocxRGB(*brand))
    _runs(document.add_paragraph(), subtitle, size=12, color=muted)
    intro = document.add_paragraph()
    intro.paragraph_format.left_indent = Cm(0.6)
    _runs(intro, note, size=10, color=muted)

    for number, path in enumerate(sources, start=1):
        pending = []
        for line in read(folder, path).splitlines() + [""]:
            if line.startswith("|"):
                pending.append(line)
                continue
            if pending:
                _table(document, pending, table_style)
                pending = []
            if line.startswith("# "):
                heading = document.add_heading(f"§{line[2:]}", level=1)
                if number > 1:
                    heading.paragraph_format.page_break_before = True
                _runs(document.add_paragraph(), f"From {path}", size=9, color=muted, italic=True)
            elif line.startswith("## "):
                document.add_heading(line[3:], level=2)
            elif line.startswith("> ") or line.strip() == "---":
                continue  # the note's own editing hint; the "From" line replaces it
            elif line.startswith("- "):
                _runs(document.add_paragraph(style="List Bullet"), line[2:])
            elif re.match(r"^\d+\. ", line):
                _runs(document.add_paragraph(style="List Number"), re.sub(r"^\d+\. ", "", line))
            elif line.startswith("Sources:") or (line and line[0] in "¹²³"):
                _runs(document.add_paragraph(), line, size=9, color=muted)
            elif line.strip():
                _runs(document.add_paragraph(), line)
    out = io.BytesIO()
    document.save(out)
    return out.getvalue()

def docx_to_pdf(data):
    """The Word document as a PDF, exported by Word itself (through COM, so
    Windows with Word installed), so the PDF is the document as Word lays it
    out."""
    with tempfile.TemporaryDirectory() as folder:
        source = os.path.join(folder, "narrative.docx")
        target = os.path.join(folder, "narrative.pdf")
        with open(source, "wb") as f:
            f.write(data)
        # 17: wdExportFormatPDF. Opened read-only, closed without saving.
        script = (
            "$word = New-Object -ComObject Word.Application; $word.Visible = $false; "
            f"try {{ $document = $word.Documents.Open('{source}', $false, $true); "
            f"$document.ExportAsFixedFormat('{target}', 17); $document.Close($false) }} "
            "finally { $word.Quit() }"
        )
        result = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script], capture_output=True, text=True)
        if result.returncode != 0 or not os.path.exists(target):
            raise RuntimeError(f"Word could not export the narrative as a PDF (it needs Windows with Word):\n{result.stderr}")
        with open(target, "rb") as f:
            return f.read()

# ---------------------------------------------------------------------------
# Deck (PowerPoint)
# ---------------------------------------------------------------------------

class Deck:
    """A 16:9 deck of blank slides with a brand bar, a title, a footer that
    names the narrative section, and speaker notes."""

    def __init__(self, *, brand, soft, footer, properties, images):
        self.brand = RGBColor(*brand)
        self.soft = RGBColor(*soft)
        self.footer = footer
        self.images = images
        self.pptx = Presentation()
        self.pptx.slide_width = Inches(13.333)
        self.pptx.slide_height = Inches(7.5)
        for key, value in properties.items():
            setattr(self.pptx.core_properties, key, value)
        self.count = 0

    def _blank(self):
        self.count += 1
        return self.pptx.slides.add_slide(self.pptx.slide_layouts[6])

    def _rect(self, slide, shape, x, y, w, h, color):
        rect = slide.shapes.add_shape(shape, x, y, w, h)
        rect.fill.solid()
        rect.fill.fore_color.rgb = color
        rect.line.fill.background()
        return rect

    def text(self, slide, x, y, w, h, paragraphs, size=20, color=None, bold=False, align=PP_ALIGN.LEFT):
        """Text boxes; **bold** spans inside a paragraph."""
        color = color or RGBColor(*INK)
        frame = slide.shapes.add_textbox(Inches(x), Inches(y), Inches(w), Inches(h)).text_frame
        frame.word_wrap = True
        frame.vertical_anchor = MSO_ANCHOR.TOP
        for i, item in enumerate(paragraphs):
            paragraph = frame.paragraphs[0] if i == 0 else frame.add_paragraph()
            paragraph.alignment = align
            for part in re.split(r"(\*\*[^*]+\*\*)", item):
                if not part:
                    continue
                run = paragraph.add_run()
                run.text = part[2:-2] if part.startswith("**") else part
                run.font.size = PPt(size)
                run.font.color.rgb = color
                run.font.bold = bold or part.startswith("**")
                run.font.name = "Segoe UI"
            paragraph.space_after = PPt(size * 0.6)

    def title_slide(self, title, line, byline, notes, title_size=60):
        slide = self._blank()
        self._rect(slide, MSO_SHAPE.RECTANGLE, 0, 0, self.pptx.slide_width, self.pptx.slide_height, self.brand)
        white = RGBColor(255, 255, 255)
        self.text(slide, 0.9, 2.1, 11.5, 1.2, [title], size=title_size, bold=True, color=white)
        self.text(slide, 0.9, 3.4, 11.5, 0.9, [line], size=30, color=white)
        self.text(slide, 0.9, 5.6, 11.5, 0.9, byline, size=16, color=white)
        slide.notes_slide.notes_text_frame.text = notes
        return slide

    def slide(self, title, section, notes):
        slide = self._blank()
        self._rect(slide, MSO_SHAPE.RECTANGLE, 0, 0, Inches(0.18), self.pptx.slide_height, self.brand)
        muted = RGBColor(*MUTED)
        self.text(slide, 0.7, 0.45, 12, 0.9, [title], size=34, bold=True)
        self.text(slide, 0.7, 6.85, 9, 0.4, [f"{self.footer} · from the narrative, {section}"], size=11, color=muted)
        self.text(slide, 11.6, 6.85, 1.2, 0.4, [str(self.count)], size=11, color=muted, align=PP_ALIGN.RIGHT)
        slide.notes_slide.notes_text_frame.text = notes
        return slide

    def picture(self, slide, path, x=0.6, y=1.45, w=8.4):
        slide.shapes.add_picture(io.BytesIO(self.images[path]), Inches(x), Inches(y), width=Inches(w))

    def card(self, slide, x, y, value, label, w=3.6, h=1.15, size=28):
        shape = self._rect(slide, MSO_SHAPE.ROUNDED_RECTANGLE, Inches(x), Inches(y), Inches(w), Inches(h), self.soft)
        shape.adjustments[0] = 0.12
        self.text(slide, x + 0.25, y + 0.1, w - 0.4, 0.6, [value], size=size, bold=True, color=self.brand)
        self.text(slide, x + 0.25, y + 0.65, w - 0.4, h - 0.7, [label], size=13, color=RGBColor(*MUTED))

    def arrow(self, slide, x, y):
        self._rect(slide, MSO_SHAPE.RIGHT_ARROW, Inches(x), Inches(y), Inches(0.16), Inches(0.2), self.brand)

    def save(self):
        out = io.BytesIO()
        self.pptx.save(out)
        return out.getvalue()

# ---------------------------------------------------------------------------
# Fixture modules
# ---------------------------------------------------------------------------

def _literal(text):
    return "`" + text.replace("\\", "\\\\").replace("`", "\\`").replace("${", "\\${") + "`"

def _const(path):
    return re.sub(r"[^A-Z0-9]+", "_", path.upper()).strip("_")

def _base64(data):
    encoded = base64.b64encode(data).decode()
    return "\n".join(encoded[i:i + 120] for i in range(0, len(encoded), 120))

def write_fixtures(prefix, header, folder, text_files, binary_files):
    """<prefix>-presentations.ts (the text files, and the binaries decoded)
    and <prefix>-presentation-binaries.ts (the binaries as base64)."""
    with open(os.path.join(FIXTURES, f"{prefix}-presentation-binaries.ts"), "w", encoding="utf-8", newline="\n") as f:
        f.write("/// Generated by platform/web/scripts/demo-presentations; do not edit by hand.\n")
        f.write("/// The Presentations workspace's binary files (the Word narrative, the\n")
        f.write(f"/// PowerPoint deck, and its charts) as base64. See {prefix}-presentations.ts.\n")
        f.write("export const PRESENTATION_BINARIES: Record<string, string> = {\n")
        for path, data in binary_files.items():
            f.write(f'  "{path}": `\n{_base64(data)}\n`,\n')
        f.write("};\n")

    with open(os.path.join(FIXTURES, f"{prefix}-presentations.ts"), "w", encoding="utf-8", newline="\n") as f:
        f.write("/// Generated by platform/web/scripts/demo-presentations; edit the notes there.\n")
        f.write("".join(f"/// {line}\n" if line else "///\n" for line in header.strip().splitlines()))
        f.write('import { base64ToBytes } from "../store";\n')
        f.write(f'import {{ PRESENTATION_BINARIES }} from "./{prefix}-presentation-binaries";\n\n')
        for path in text_files:
            f.write(f"const {_const(path)} = {_literal(read(folder, path))};\n\n")
        f.write("export const PRESENTATION_FILES: Record<string, string | Uint8Array> = {\n")
        for path in text_files:
            f.write(f'  "{path}": {_const(path)},\n')
        f.write("  ...Object.fromEntries(\n")
        f.write('    Object.entries(PRESENTATION_BINARIES).map(([path, base64]) => [path, base64ToBytes(base64.replace(/\\s/g, ""))]),\n')
        f.write("  ),\n};\n")

    for path, data in binary_files.items():
        print(f"{path}: {len(data):,} bytes")

//! Shared fixture for the tool-media live tests: a `view_images` tool whose
//! result hands the model two images and one PDF as media entries, and the
//! answer shape every provider must produce from them.

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, FunctionToolSpec, RunId, ToolCallId, ToolExecutionSpec, ToolKind, ToolName,
    ToolParallelism, ToolSpec, TurnId,
    media::{MediaKind, media_context_entry, media_handle, tool_media_line},
    storage::{BlobStore, InMemoryBlobStore},
};
use serde_json::json;

/// 32x32 solid red PNG.
pub const RED_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg==";
/// 32x32 solid blue PNG.
pub const BLUE_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAJklEQVR42u3NsQkAAAjAsP7/tF7hIASyp5pjAoFAIBAIBAKB4EmwOkv8Lom8x/sAAAAASUVORK5CYII=";

pub const PDF_NUMBER: &str = "4711";

/// Which assets the tool result hands over. Claude Opus 5's refusal
/// classifier rejects a PDF document that arrives after a tool round-trip
/// (other Anthropic models and both OpenAI APIs read it), so the Anthropic
/// suite covers images and PDFs on different models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolMediaSet {
    ImagesAndPdf,
    ImagesOnly,
    PdfOnly,
    SingleImage,
}

impl ToolMediaSet {
    pub const fn prompt(self) -> &'static str {
        match self {
            Self::ImagesAndPdf => {
                "Please call the view_images tool once; it shows you two images and one PDF \
document from our design review. Then tell me, in a short reply: the dominant color of image 1, \
the dominant color of image 2, the number written in the PDF document, and the media handle \
(it starts with media:) of the blue image so I can look it up."
            }
            Self::ImagesOnly => {
                "Please call the view_images tool once; it shows you two images from our design \
review. Then tell me, in a short reply: the dominant color of image 1, the dominant color of \
image 2, and the media handle (it starts with media:) of the blue image so I can look it up."
            }
            Self::PdfOnly => {
                "Please call the view_images tool once; it shows you one PDF document from our \
design review. Then tell me, in a short reply, the number written in the document and its media \
handle (it starts with media:) so I can look it up."
            }
            Self::SingleImage => {
                "Call view_images once, then tell me the dominant color of the image it shows \
and the image's media handle (it starts with media:)."
            }
        }
    }
}

pub const TOOL_MEDIA_PROMPT: &str = ToolMediaSet::ImagesAndPdf.prompt();

pub async fn view_images_tool_spec(blobs: &InMemoryBlobStore) -> ToolSpec {
    let schema_ref = llm_runtime::blob_io::put_json(
        blobs,
        &json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    )
    .await
    .expect("schema");
    ToolSpec {
        name: ToolName::try_new("view_images").expect("tool name"),
        kind: ToolKind::Function(FunctionToolSpec {
            description_ref: Some(
                blobs
                    .insert_text("Show the rendered images and the brief document.")
                    .await,
            ),
            input_schema_ref: schema_ref,
            output_schema_ref: None,
            strict: Some(true),
            provider_options_ref: None,
        }),
        parallelism: ToolParallelism::ParallelSafe,
        execution: ToolExecutionSpec::default(),
    }
}

pub struct ToolMediaFixture {
    /// The tool result entry followed by its media entries.
    pub entries: Vec<ContextEntry>,
    pub set: ToolMediaSet,
    pub red_handle: String,
    pub blue_handle: String,
    pub pdf_handle: String,
}

/// The entries a tool activity commits for `call_id`: the visible result
/// announcing the assets by position and handle, then a red PNG, a blue
/// PNG, and/or a one-page PDF as tool-sourced media entries.
pub async fn tool_media_entries(
    blobs: &InMemoryBlobStore,
    call_id: ToolCallId,
    first_entry_id: u64,
) -> ToolMediaFixture {
    tool_media_entries_for(blobs, call_id, first_entry_id, ToolMediaSet::ImagesAndPdf).await
}

pub async fn tool_media_entries_for(
    blobs: &InMemoryBlobStore,
    call_id: ToolCallId,
    first_entry_id: u64,
    set: ToolMediaSet,
) -> ToolMediaFixture {
    use base64::Engine as _;
    let red = base64::engine::general_purpose::STANDARD
        .decode(RED_PNG_BASE64)
        .expect("red png");
    let blue = base64::engine::general_purpose::STANDARD
        .decode(BLUE_PNG_BASE64)
        .expect("blue png");
    let pdf = minimal_pdf(&format!("Brief number {PDF_NUMBER}"));
    let red_ref = blobs.put_bytes(red.clone()).await.expect("store red");
    let blue_ref = blobs.put_bytes(blue.clone()).await.expect("store blue");
    let pdf_ref = blobs.put_bytes(pdf.clone()).await.expect("store pdf");
    let images = matches!(set, ToolMediaSet::ImagesAndPdf | ToolMediaSet::ImagesOnly);
    let single = set == ToolMediaSet::SingleImage;
    let document = matches!(set, ToolMediaSet::ImagesAndPdf | ToolMediaSet::PdfOnly);
    let mut lines = Vec::new();
    if images || single {
        lines.push(tool_media_line(
            MediaKind::Image,
            1,
            &red_ref,
            "image/png",
            None,
            red.len() as u64,
        ));
    }
    if images {
        lines.push(tool_media_line(
            MediaKind::Image,
            2,
            &blue_ref,
            "image/png",
            None,
            blue.len() as u64,
        ));
    }
    if document {
        lines.push(tool_media_line(
            MediaKind::Document,
            1,
            &pdf_ref,
            "application/pdf",
            Some("brief.pdf"),
            pdf.len() as u64,
        ));
    }
    let visible = lines.join("\n");
    let visible_ref = blobs.insert_text(&visible).await;
    let source = ContextEntrySource::Tool {
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        batch_id: None,
    };
    let commit = |id: u64, input: engine::ContextEntryInput| ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(id),
        kind: input.kind,
        source: source.clone(),
        content: input.content,
        preview: input.preview,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    };
    let mut entries = vec![ContextEntry {
        key: None,
        entry_id: ContextEntryId::new(first_entry_id),
        kind: ContextEntryKind::ToolResult {
            call_id,
            is_error: false,
        },
        source: source.clone(),
        content: engine::ContentRef::text(visible_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }];
    let mut next_id = first_entry_id + 1;
    let mut push = |entries: &mut Vec<ContextEntry>, input: engine::ContextEntryInput| {
        entries.push(commit(next_id, input));
        next_id += 1;
    };
    if images || single {
        push(
            &mut entries,
            media_context_entry(red_ref.clone(), "image/png", MediaKind::Image, None),
        );
    }
    if images {
        push(
            &mut entries,
            media_context_entry(blue_ref.clone(), "image/png", MediaKind::Image, None),
        );
    }
    if document {
        push(
            &mut entries,
            media_context_entry(
                pdf_ref.clone(),
                "application/pdf",
                MediaKind::Document,
                Some("brief.pdf"),
            ),
        );
    }
    ToolMediaFixture {
        entries,
        set,
        red_handle: media_handle(&red_ref),
        blue_handle: media_handle(&blue_ref),
        pdf_handle: media_handle(&pdf_ref),
    }
}

/// The model must have told the two images apart (red is image 1, so it is
/// named first) and named the blue image by its handle; with a PDF in the
/// set it must also have read the number in it.
pub fn assert_tool_media_answer(text: &str, fixture: &ToolMediaFixture) {
    let lower = text.to_lowercase();
    match fixture.set {
        ToolMediaSet::SingleImage => {
            assert!(lower.contains("red"), "the image is red: {text:?}");
            assert!(
                lower.contains(&fixture.red_handle),
                "the answer should name the image by {}: {text:?}",
                fixture.red_handle
            );
        }
        ToolMediaSet::ImagesAndPdf | ToolMediaSet::ImagesOnly => {
            let red = lower
                .find("red")
                .unwrap_or_else(|| panic!("expected red in {text:?}"));
            let blue = lower
                .find("blue")
                .unwrap_or_else(|| panic!("expected blue in {text:?}"));
            assert!(
                red < blue,
                "image 1 is red and should be named first: {text:?}"
            );
            assert!(
                lower.contains(&fixture.blue_handle),
                "the answer should name the blue image by {}: {text:?}",
                fixture.blue_handle
            );
        }
        ToolMediaSet::PdfOnly => {
            assert!(
                lower.contains(&fixture.pdf_handle),
                "the answer should name the document by {}: {text:?}",
                fixture.pdf_handle
            );
        }
    }
    if matches!(
        fixture.set,
        ToolMediaSet::ImagesAndPdf | ToolMediaSet::PdfOnly
    ) {
        assert!(
            lower.contains(PDF_NUMBER),
            "the answer should quote {PDF_NUMBER} from the PDF: {text:?}"
        );
    }
}

pub fn assistant_entry(execution: &llm_runtime::LlmGenerationExecution) -> engine::ContentRef {
    execution
        .result
        .context_entries
        .iter()
        .find_map(|item| match item.kind {
            ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            } => Some(item.content.clone()),
            _ => None,
        })
        .expect("assistant context item")
}

/// Commit a generation's output entries as retained assistant context.
pub fn retained(first_entry_id: u64, outputs: &[engine::ContextEntryInput]) -> Vec<ContextEntry> {
    outputs
        .iter()
        .enumerate()
        .map(|(index, item)| ContextEntry {
            key: None,
            entry_id: ContextEntryId::new(first_entry_id + index as u64),
            kind: item.kind.clone(),
            source: match item.kind {
                ContextEntryKind::ReasoningState => ContextEntrySource::Reasoning {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
                _ => ContextEntrySource::AssistantOutput {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                },
            },
            content: item.content.clone(),
            preview: item.preview.clone(),
            origin: None,
            provenance_ref: item.provenance_ref.clone(),
            token_estimate: item.token_estimate.clone(),
            supersedes: None,
        })
        .collect()
}

pub fn blob_ref_of(bytes: &[u8]) -> BlobRef {
    BlobRef::from_bytes(bytes)
}

/// A minimal one-page PDF with correct xref offsets carrying `text`.
pub fn minimal_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref_offset = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        objects.len() + 1
    ));
    pdf.into_bytes()
}

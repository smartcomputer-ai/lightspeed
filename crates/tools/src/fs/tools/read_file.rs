//! Canonical read-file operation.

use engine::{
    BlobRef,
    media::{MediaKind, admit_tool_media, media_handle, sniff_media_type, tool_media_line},
};
use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{FsError, FsPath, FsToolContext},
    runtime::ToolMediaOutput,
};

use super::{invalid_request, resolve_path};

pub const DEFAULT_READ_FILE_LINE_LIMIT: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadFileArgs {
    pub path: FsPath,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    /// Empty for a media file; the bytes are in `media`.
    pub text: String,
    /// For a media file: the one-line announcement of the media the model
    /// gets to see, with its handle.
    pub line_numbered_text: String,
    pub line_start: usize,
    pub line_count: usize,
    pub total_lines: usize,
    pub truncated: bool,
    pub bytes_read: usize,
    /// Set when the file is an image or PDF: the model sees the bytes as
    /// provider-native media instead of text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<ReadFileMedia>,
}

/// An image or PDF read as media rather than text; the bytes are in CAS.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadFileMedia {
    pub content_ref: BlobRef,
    pub media_type: String,
    pub kind: MediaKind,
    /// `media:` plus the first twelve hex characters of `content_ref`.
    pub handle: String,
    pub byte_len: u64,
}

impl ReadFileResult {
    /// The media the tool hands the model, named after the file.
    pub fn media_outputs(&self) -> Vec<ToolMediaOutput> {
        self.media
            .iter()
            .map(|media| ToolMediaOutput {
                content_ref: media.content_ref.clone(),
                media_type: media.media_type.clone(),
                kind: media.kind,
                name: file_name(&self.resolved_path),
            })
            .collect()
    }
}

fn file_name(path: &FsPath) -> Option<String> {
    path.as_str()
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

pub async fn invoke_read_file(
    ctx: &FsToolContext,
    args: ReadFileArgs,
) -> ToolResult<ReadFileResult> {
    let resolved_path = resolve_path(ctx, &args.path)?;
    let offset = args.offset.unwrap_or(1);
    if offset == 0 {
        return Err(invalid_request("read_file offset must be 1 or greater"));
    }
    let limit = args.limit.unwrap_or(DEFAULT_READ_FILE_LINE_LIMIT);
    if limit == 0 {
        return Err(invalid_request("read_file limit must be 1 or greater"));
    }

    // Ranged read: an oversized file is truncated at the source (when the
    // backend supports ranges) and rejected with its true size instead of
    // being transferred in full.
    let read = ctx
        .fs
        .read_file_range(
            &resolved_path,
            0,
            Some(ctx.limits.max_file_read_bytes.saturating_add(1)),
        )
        .await?;
    if read.truncated || read.file_size > ctx.limits.max_file_read_bytes {
        return Err(invalid_request(format!(
            "read_file would read {} bytes, exceeding max_file_read_bytes={}",
            read.file_size, ctx.limits.max_file_read_bytes
        )));
    }
    let bytes_read = read.bytes.len();

    // Images and PDFs reach the model as media, not text. They are
    // recognized by content, bounded like every tool-produced asset, and
    // announced by handle; `offset` and `limit` do not apply.
    if let Some(media_type) = sniff_media_type(&read.bytes) {
        let byte_len = read.bytes.len() as u64;
        let kind = admit_tool_media(Some(media_type), byte_len).map_err(|rejection| {
            invalid_request(format!(
                "read_file cannot hand {resolved_path} to the model: {rejection}"
            ))
        })?;
        let content_ref = ctx.blobs.put_bytes(read.bytes).await?;
        let handle = media_handle(&content_ref);
        let mut line = tool_media_line(
            kind,
            1,
            &content_ref,
            media_type,
            file_name(&resolved_path).as_deref(),
            byte_len,
        );
        if args.offset.is_some() || args.limit.is_some() {
            line.push_str(" (offset and limit do not apply to media files)");
        }
        return Ok(ReadFileResult {
            path: args.path,
            resolved_path,
            text: String::new(),
            line_numbered_text: line,
            line_start: 1,
            line_count: 0,
            total_lines: 0,
            truncated: false,
            bytes_read,
            media: Some(ReadFileMedia {
                content_ref,
                media_type: media_type.to_owned(),
                kind,
                handle,
                byte_len,
            }),
        });
    }

    let contents = String::from_utf8(read.bytes).map_err(FsError::invalid_data)?;
    let lines = contents.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    let start_index = offset - 1;
    let selected = lines
        .iter()
        .enumerate()
        .skip(start_index)
        .take(limit)
        .map(|(index, line)| (index + 1, *line))
        .collect::<Vec<_>>();

    let text = selected
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    let line_numbered_text = selected
        .iter()
        .map(|(line_number, line)| format!("{line_number:>6} | {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let line_count = selected.len();
    let truncated = start_index < total_lines && start_index + line_count < total_lines;

    Ok(ReadFileResult {
        path: args.path,
        resolved_path,
        text,
        line_numbered_text,
        line_start: offset,
        line_count,
        total_lines,
        truncated,
        bytes_read,
        media: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        limits::ToolLimits,
    };

    fn context(fs: Arc<dyn FileSystem>) -> FsToolContext {
        FsToolContext::new(fs, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_resolves_relative_paths_against_context_cwd() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace").expect("dir"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create directory");
        fs.write_file(
            &FsPath::new("/workspace/file.txt").expect("file path"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("file.txt").expect("relative path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read file");

        assert_eq!(
            result.resolved_path,
            FsPath::new("/workspace/file.txt").unwrap()
        );
        assert_eq!(result.text, "hello");
        assert_eq!(result.line_numbered_text, "     1 | hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_applies_offset_and_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file path"),
            b"one\ntwo\nthree\nfour".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs));

        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                offset: Some(2),
                limit: Some(2),
            },
        )
        .await
        .expect("read file");

        assert_eq!(result.text, "two\nthree");
        assert_eq!(result.line_numbered_text, "     2 | two\n     3 | three");
        assert_eq!(result.line_start, 2);
        assert_eq!(result.line_count, 2);
        assert_eq!(result.total_lines, 4);
        assert!(result.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_enforces_max_file_read_bytes() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file path"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_file_read_bytes: 4,
            ..ToolLimits::default()
        });

        let error = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect_err("read should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    /// A backend with native range support truncates at the source; the tool
    /// must reject the oversized file using the reported true size without a
    /// full transfer.
    struct RangedOnlyFileSystem {
        file_size: u64,
    }

    #[async_trait::async_trait]
    impl FileSystem for RangedOnlyFileSystem {
        fn access_policy(&self) -> crate::fs::FileAccessPolicy {
            InMemoryFileSystem::full_access().access_policy()
        }

        async fn read_file(&self, _path: &FsPath) -> crate::fs::FsResult<Vec<u8>> {
            panic!("ranged backend must not be asked for a full transfer");
        }

        async fn read_file_range(
            &self,
            _path: &FsPath,
            offset: u64,
            max_bytes: Option<u64>,
        ) -> crate::fs::FsResult<crate::fs::FsRangedRead> {
            let take = max_bytes.expect("read tool always bounds the range") as usize;
            let returned = take.min((self.file_size - offset) as usize);
            Ok(crate::fs::FsRangedRead {
                bytes: vec![b'a'; returned],
                file_size: self.file_size,
                truncated: (offset + returned as u64) < self.file_size,
            })
        }

        async fn write_file(&self, _path: &FsPath, _contents: Vec<u8>) -> crate::fs::FsResult<()> {
            unimplemented!()
        }

        async fn create_directory(
            &self,
            _path: &FsPath,
            _options: CreateDirectoryOptions,
        ) -> crate::fs::FsResult<()> {
            unimplemented!()
        }

        async fn get_metadata(
            &self,
            _path: &FsPath,
        ) -> crate::fs::FsResult<crate::fs::FileMetadata> {
            unimplemented!()
        }

        async fn read_directory(
            &self,
            _path: &FsPath,
        ) -> crate::fs::FsResult<Vec<crate::fs::ReadDirectoryEntry>> {
            unimplemented!()
        }

        async fn remove(
            &self,
            _path: &FsPath,
            _options: crate::fs::RemoveOptions,
        ) -> crate::fs::FsResult<()> {
            unimplemented!()
        }

        async fn copy(
            &self,
            _source_path: &FsPath,
            _destination_path: &FsPath,
            _options: crate::fs::CopyOptions,
        ) -> crate::fs::FsResult<()> {
            unimplemented!()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_rejects_oversized_files_without_full_transfer() {
        let ctx =
            context(Arc::new(RangedOnlyFileSystem { file_size: 100 })).with_limits(ToolLimits {
                max_file_read_bytes: 10,
                ..ToolLimits::default()
            });

        let error = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/huge.bin").expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect_err("oversized read should fail");

        let ToolError::InvalidRequest { message } = error else {
            panic!("expected invalid request");
        };
        assert!(message.contains("100 bytes"), "message: {message}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_accepts_a_file_exactly_at_the_cap() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").expect("path"), b"12345".to_vec())
            .await
            .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_file_read_bytes: 5,
            ..ToolLimits::default()
        });

        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read at cap");

        assert_eq!(result.text, "12345");
        assert_eq!(result.bytes_read, 5);
    }

    async fn media_context() -> (Arc<InMemoryFileSystem>, FsToolContext) {
        let fs = Arc::new(InMemoryFileSystem::full_access());
        let ctx = FsToolContext::new(fs.clone(), Arc::new(InMemoryBlobStore::new()));
        (fs, ctx)
    }

    async fn write(fs: &InMemoryFileSystem, path: &str, bytes: Vec<u8>) {
        if let Some((parent, _)) = path.rsplit_once('/')
            && !parent.is_empty()
        {
            let _ = fs
                .create_directory(
                    &FsPath::new(parent).expect("parent"),
                    CreateDirectoryOptions::single(),
                )
                .await;
        }
        fs.write_file(&FsPath::new(path).expect("path"), bytes)
            .await
            .expect("write file");
    }

    async fn read(ctx: &FsToolContext, path: &str) -> ToolResult<ReadFileResult> {
        invoke_read_file(
            ctx,
            ReadFileArgs {
                path: FsPath::new(path).expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn images_and_pdfs_are_read_as_media_named_by_handle() {
        let (fs, ctx) = media_context().await;
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[7u8; 100]);
        let fixtures: [(&str, Vec<u8>, &str, MediaKind); 5] = [
            (
                "/shots/photo.png",
                png.clone(),
                "image/png",
                MediaKind::Image,
            ),
            (
                "/shots/photo.jpg",
                b"\xff\xd8\xff\xe0 jpeg".to_vec(),
                "image/jpeg",
                MediaKind::Image,
            ),
            (
                "/shots/anim.gif",
                b"GIF89a gif".to_vec(),
                "image/gif",
                MediaKind::Image,
            ),
            (
                "/shots/pic.webp",
                b"RIFF\0\0\0\0WEBPVP8 ".to_vec(),
                "image/webp",
                MediaKind::Image,
            ),
            (
                "/docs/report.pdf",
                b"%PDF-1.7 report".to_vec(),
                "application/pdf",
                MediaKind::Document,
            ),
        ];
        for (path, bytes, media_type, kind) in fixtures {
            write(&fs, path, bytes.clone()).await;
            let result = read(&ctx, path).await.expect("read media");
            let media = result.media.clone().expect("media");
            assert_eq!(media.media_type, media_type, "{path}");
            assert_eq!(media.kind, kind, "{path}");
            assert_eq!(media.byte_len, bytes.len() as u64);
            assert_eq!(media.content_ref, BlobRef::from_bytes(&bytes));
            assert_eq!(media.handle, media_handle(&media.content_ref));
            assert_eq!(
                ctx.blobs
                    .read_bytes(&media.content_ref)
                    .await
                    .expect("stored"),
                bytes,
                "bytes land in CAS"
            );
            assert!(result.text.is_empty());
            let name = path.rsplit('/').next().unwrap();
            assert!(
                result.line_numbered_text.starts_with(&format!(
                    "[{} 1 · {} · {media_type} · {name} · ",
                    kind.label(),
                    media.handle
                )),
                "{}",
                result.line_numbered_text
            );
            let outputs = result.media_outputs();
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].name.as_deref(), Some(name));
            let entry = outputs[0].context_entry();
            assert_eq!(
                entry.preview.as_deref(),
                Some(format!("[{}: {name}]", kind.label())).as_deref()
            );
            assert_eq!(entry.content.media_type.as_deref(), Some(media_type));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_and_undecodable_files_behave_as_before() {
        let (fs, ctx) = media_context().await;
        write(&fs, "/notes.txt", b"one\ntwo".to_vec()).await;
        let result = read(&ctx, "/notes.txt").await.expect("read text");
        assert!(result.media.is_none());
        assert_eq!(result.text, "one\ntwo");
        assert!(result.media_outputs().is_empty());

        write(&fs, "/archive.zip", b"PK\x03\x04\xff\xfe binary".to_vec()).await;
        let error = read(&ctx, "/archive.zip")
            .await
            .expect_err("zip is not media");
        assert!(
            matches!(error, ToolError::Filesystem(FsError::InvalidData { .. })),
            "{error:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn media_offset_and_limit_are_ignored_with_a_note() {
        let (fs, ctx) = media_context().await;
        write(&fs, "/photo.png", b"\x89PNG\r\n\x1a\n....".to_vec()).await;
        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/photo.png").expect("path"),
                offset: Some(3),
                limit: Some(2),
            },
        )
        .await
        .expect("read media");
        assert!(result.media.is_some());
        assert!(
            result
                .line_numbered_text
                .ends_with("(offset and limit do not apply to media files)"),
            "{}",
            result.line_numbered_text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_media_is_an_error_result_with_its_size() {
        let (fs, ctx) = media_context().await;
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(engine::media::MAX_TOOL_MEDIA_BYTES as usize + 1, 0);
        write(&fs, "/huge.png", png).await;
        let error = read(&ctx, "/huge.png").await.expect_err("too large");
        let message = error.to_string();
        assert!(message.contains("exceeds the"), "{message}");
        assert!(message.contains("/huge.png"), "{message}");
    }
}

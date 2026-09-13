use std::sync::Arc;
use tools::skills::SkillId;

use engine::{
    ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource, ContextMessageRole,
    ContextSnapshot, LlmGenerationRequest, LlmRequest, ModelSelection, ProviderApiKind, RunId,
    SessionId, TurnId,
    storage::{BlobStore, InMemoryBlobStore},
};
use llm_runtime::{LlmGenerationAdapter, OpenAiCompletionsLlmAdapter};
use tools::skills::{
    SkillCatalogSnapshot, SkillDependencies, SkillLocation, SkillMetadata, SkillScope, SkillSource,
    SkillTrustLevel,
};
use vfs::{VfsPath, VfsWorkspaceId};

mod support;

use support::{
    openai_completions_live_client, openai_completions_live_model,
    retrying_openai_completions_client,
};

fn entry(id: u64, kind: ContextEntryKind, content_ref: engine::BlobRef) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(id),
        key: None,
        kind,
        source: ContextEntrySource::Runtime {
            label: "live-skill".to_owned(),
        },
        content: engine::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

fn request(entries: Vec<ContextEntry>) -> LlmGenerationRequest {
    LlmGenerationRequest {
        session_id: SessionId::new("session-openai-completions-skills-live"),
        run_id: RunId::new(1),
        turn_id: TurnId::new(1),
        request: LlmRequest {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiCompletions,
                provider_id: "openai".to_owned(),
                model: openai_completions_live_model(),
            },
            request_fingerprint: "openai-completions-skills-live".to_owned(),
            context: ContextSnapshot {
                api_kind: ProviderApiKind::OpenAiCompletions,
                context_revision: 1,
                entries,
                token_estimate: None,
            },
            tools: Vec::new(),
            tool_choice: None,
            output_limit: Some(256),
            reasoning_effort: None,
            parallel_tool_use: None,
            processing_tier: None,
            provider_response_id: None,
            compaction: None,
            params: None,
        },
    }
}

async fn answer(blobs: Arc<InMemoryBlobStore>, request: LlmGenerationRequest) -> String {
    let adapter = OpenAiCompletionsLlmAdapter::new(
        retrying_openai_completions_client(openai_completions_live_client()),
        blobs.clone(),
    );
    let execution = adapter.generate(request).await.expect("generate");
    support::content_text(blobs.as_ref(), &execution.result.context_entries[0].content).await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY (costs real money)"]
async fn openai_completions_runtime_live_skill_catalog_exposes_relevant_skill_path() {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let skill_id = SkillId::new("skill:release-audit");
    let workspace_id = VfsWorkspaceId::new("workspace-live-skills");
    let catalog = SkillCatalogSnapshot::new(
        vec![SkillMetadata {
            skill_id: skill_id.clone(),
            name: "release-audit".to_owned(),
            description: "Audit a software release for missing changelog and migration steps."
                .to_owned(),
            short_description: Some("Release safety audit".to_owned()),
            source: SkillSource::Workspace {
                root_id: "root-live".to_owned(),
                workspace_id: workspace_id.clone(),
            },
            scope: SkillScope::Global,
            enabled: true,
            trust: SkillTrustLevel::Project,
            interface: None,
            dependencies: SkillDependencies::default(),
            location: SkillLocation::AttachedWorkspace {
                workspace_id,
                source_attachment_path: VfsPath::parse("/skills").expect("attachment path"),
                skill_dir_path: VfsPath::parse("/skills/release-audit").expect("skill path"),
                skill_doc_path: VfsPath::parse("/skills/release-audit/SKILL.md")
                    .expect("skill doc path"),
            },
        }],
        Vec::new(),
    );
    let catalog_ref = blobs
        .put_bytes(serde_json::to_vec(&catalog).expect("catalog JSON"))
        .await
        .expect("store catalog");
    let catalog_input =
        tools::skills::skill_catalog_context_input(blobs.as_ref(), &catalog, catalog_ref)
            .await
            .expect("render catalog");
    let mut catalog_entry = entry(1, catalog_input.kind, catalog_input.content.content_ref);
    catalog_entry.provenance_ref = catalog_input.provenance_ref;
    let user = blobs
        .insert_text(
            "I need to check a release for missing migration and changelog work. Which skill should I read, and at what exact path?",
        )
        .await;

    let output = answer(
        blobs.clone(),
        request(vec![
            catalog_entry,
            entry(
                2,
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                user,
            ),
        ]),
    )
    .await
    .to_lowercase();

    assert!(output.contains("release-audit"), "output: {output}");
    assert!(
        output.contains("/skills/release-audit/skill.md"),
        "output: {output}"
    );
}

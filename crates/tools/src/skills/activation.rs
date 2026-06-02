use engine::{
    BlobRef, SkillActivation, SkillActivationScope, SkillActivationSource, ToolCallId,
    ToolCallStatus, ToolExecutionTarget, ToolName,
};
use serde_json::Value;

use crate::{
    host::tools::ReadFileResult,
    skills::{SkillCatalogSnapshot, SkillLocation, SkillMetadata},
};

pub struct SkillToolResultActivationInput<'a> {
    pub catalog_ref: &'a BlobRef,
    pub catalog: &'a SkillCatalogSnapshot,
    pub current_activations: &'a [SkillActivation],
    pub call_id: &'a ToolCallId,
    pub tool_name: &'a ToolName,
    pub status: ToolCallStatus,
    pub execution_target: Option<&'a ToolExecutionTarget>,
    pub output_json: &'a Value,
}

pub fn skill_activation_from_tool_result(
    input: SkillToolResultActivationInput<'_>,
) -> Option<SkillActivation> {
    if input.status != ToolCallStatus::Succeeded || !is_read_file_tool(input.tool_name) {
        return None;
    }

    let read = serde_json::from_value::<ReadFileResult>(input.output_json.clone()).ok()?;
    if !is_complete_skill_doc_read(&read) {
        return None;
    }

    let skill = input
        .catalog
        .skills
        .iter()
        .find(|skill| skill_matches_read(skill, &read, input.execution_target))?;
    if input
        .current_activations
        .iter()
        .any(|activation| activation.skill_id == skill.skill_id)
    {
        return None;
    }

    Some(SkillActivation {
        skill_id: skill.skill_id.clone(),
        catalog_ref: input.catalog_ref.clone(),
        source: SkillActivationSource::ToolResult {
            call_id: input.call_id.clone(),
        },
        scope: SkillActivationScope::Run,
    })
}

fn is_read_file_tool(tool_name: &ToolName) -> bool {
    matches!(tool_name.as_str(), "read_file" | "Read")
}

fn is_complete_skill_doc_read(read: &ReadFileResult) -> bool {
    read.line_start == 1 && !read.truncated && read.line_count == read.total_lines
}

fn skill_matches_read(
    skill: &SkillMetadata,
    read: &ReadFileResult,
    execution_target: Option<&ToolExecutionTarget>,
) -> bool {
    match &skill.location {
        SkillLocation::MountedSnapshot { skill_doc_path, .. }
        | SkillLocation::MountedWorkspace { skill_doc_path, .. } => {
            read.resolved_path.as_str() == skill_doc_path.as_str()
        }
        SkillLocation::HostFilesystem {
            target,
            skill_doc_path,
            ..
        } => execution_target == Some(target) && read.resolved_path.as_str() == skill_doc_path,
    }
}

#[cfg(test)]
mod tests {
    use engine::{SkillId, ToolCallId, ToolCallStatus, ToolName};
    use serde_json::json;
    use vfs::VfsPath;

    use super::*;
    use crate::skills::{
        SkillDependencies, SkillLoadWarning, SkillMetadata, SkillScope, SkillSource,
        SkillTrustLevel,
    };

    #[test]
    fn activates_skill_for_complete_read_file_of_cataloged_skill_doc() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let catalog = SkillCatalogSnapshot::new(
            None,
            vec![skill("/skills/system/review/SKILL.md")],
            Vec::<SkillLoadWarning>::new(),
        );
        let activation = skill_activation_from_tool_result(SkillToolResultActivationInput {
            catalog_ref: &catalog_ref,
            catalog: &catalog,
            current_activations: &[],
            call_id: &ToolCallId::new("call_1"),
            tool_name: &ToolName::new("read_file"),
            status: ToolCallStatus::Succeeded,
            execution_target: None,
            output_json: &read_output("/skills/system/review/SKILL.md", 1, false),
        })
        .expect("activation");

        assert_eq!(activation.skill_id, SkillId::new("skill:test"));
        assert_eq!(activation.catalog_ref, catalog_ref);
        assert_eq!(
            activation.source,
            SkillActivationSource::ToolResult {
                call_id: ToolCallId::new("call_1")
            }
        );
    }

    #[test]
    fn ignores_partial_skill_doc_reads() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let catalog = SkillCatalogSnapshot::new(
            None,
            vec![skill("/skills/system/review/SKILL.md")],
            Vec::<SkillLoadWarning>::new(),
        );

        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("read_file"),
                status: ToolCallStatus::Succeeded,
                execution_target: None,
                output_json: &read_output("/skills/system/review/SKILL.md", 2, false),
            })
            .is_none()
        );
        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("read_file"),
                status: ToolCallStatus::Succeeded,
                execution_target: None,
                output_json: &read_output("/skills/system/review/SKILL.md", 1, true),
            })
            .is_none()
        );
        let mut line_limited_read = read_output("/skills/system/review/SKILL.md", 1, false);
        line_limited_read["line_count"] = json!(2);
        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("read_file"),
                status: ToolCallStatus::Succeeded,
                execution_target: None,
                output_json: &line_limited_read,
            })
            .is_none()
        );
    }

    #[test]
    fn ignores_non_cataloged_skill_doc_reads() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let catalog = SkillCatalogSnapshot::new(
            None,
            vec![skill("/skills/system/review/SKILL.md")],
            Vec::<SkillLoadWarning>::new(),
        );

        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("read_file"),
                status: ToolCallStatus::Succeeded,
                execution_target: None,
                output_json: &read_output("/skills/system/other/SKILL.md", 1, false),
            })
            .is_none()
        );
    }

    #[test]
    fn ignores_non_read_file_or_failed_tool_results() {
        let catalog_ref = BlobRef::from_bytes(b"catalog");
        let catalog = SkillCatalogSnapshot::new(
            None,
            vec![skill("/skills/system/review/SKILL.md")],
            Vec::<SkillLoadWarning>::new(),
        );

        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("grep"),
                status: ToolCallStatus::Succeeded,
                execution_target: None,
                output_json: &read_output("/skills/system/review/SKILL.md", 1, false),
            })
            .is_none()
        );
        assert!(
            skill_activation_from_tool_result(SkillToolResultActivationInput {
                catalog_ref: &catalog_ref,
                catalog: &catalog,
                current_activations: &[],
                call_id: &ToolCallId::new("call_1"),
                tool_name: &ToolName::new("read_file"),
                status: ToolCallStatus::Failed,
                execution_target: None,
                output_json: &read_output("/skills/system/review/SKILL.md", 1, false),
            })
            .is_none()
        );
    }

    fn skill(path: &str) -> SkillMetadata {
        SkillMetadata {
            skill_id: SkillId::new("skill:test"),
            name: "Review".to_owned(),
            description: "Review deploys".to_owned(),
            short_description: None,
            source: SkillSource::Snapshot {
                root_id: "system".to_owned(),
                snapshot_ref: BlobRef::from_bytes(b"snapshot"),
            },
            scope: SkillScope::Global,
            target: None,
            enabled: true,
            trust: SkillTrustLevel::System,
            interface: None,
            dependencies: SkillDependencies::default(),
            location: SkillLocation::MountedSnapshot {
                source_snapshot_ref: BlobRef::from_bytes(b"snapshot"),
                source_mount_path: VfsPath::parse("/skills/system").expect("mount path"),
                skill_dir_path: VfsPath::parse("/skills/system/review").expect("skill dir"),
                skill_doc_path: VfsPath::parse(path).expect("skill doc"),
            },
            skill_doc_ref: None,
        }
    }

    fn read_output(path: &str, line_start: usize, truncated: bool) -> Value {
        let total_lines = 3;
        json!({
            "path": path,
            "resolved_path": path,
            "text": "---\nname: Review\n---",
            "line_numbered_text": "1 | ---\n2 | name: Review\n3 | ---",
            "line_start": line_start,
            "line_count": if line_start == 1 { total_lines } else { total_lines - 1 },
            "total_lines": total_lines,
            "truncated": truncated,
            "bytes_read": 24
        })
    }
}

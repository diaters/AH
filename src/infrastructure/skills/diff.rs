use std::path::Path;

use thiserror::Error;
use tracing::warn;

use crate::domain::SkillUpdateOperation;

/// 允许 LLM 修改的 frontmatter 字段白名单
pub const FRONTMATTER_WHITELIST: &[&str] = &["name", "description", "self_updatable"];

/// v8 D19：SKILL.md body 结构校验错误
#[derive(Debug, Error)]
pub enum SkillStructureError {
    #[error("instructions must contain at least one `##` heading")]
    NoSectionHeading,
    #[error("first `##` heading must have non-empty content")]
    EmptyFirstSection,
}

/// v8 D19：校验 SKILL.md body 结构
///
/// 规则：
/// 1. 至少包含 1 个 `## ` 二级标题
/// 2. 首个 `## ` 标题下必须有非空内容（到下一个 `##` 或 body 末尾之间至少 1 行非空）
pub fn validate_skill_structure(instructions: &str) -> Result<(), SkillStructureError> {
    let lines: Vec<&str> = instructions.lines().collect();
    let first_section_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .ok_or(SkillStructureError::NoSectionHeading)?;
    // 首个 section 内容范围 = [first_section_idx + 1, 下一个 ## 或末尾)
    let end = lines
        .iter()
        .enumerate()
        .skip(first_section_idx + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    let has_non_empty = lines
        .iter()
        .skip(first_section_idx + 1)
        .take(end.saturating_sub(first_section_idx + 1))
        .any(|l| !l.trim().is_empty());
    if !has_non_empty {
        return Err(SkillStructureError::EmptyFirstSection);
    }
    Ok(())
}

/// 解析 SKILL.md，返回 frontmatter 部分和 body 部分
fn split_frontmatter(content: &str) -> (String, String) {
    let mut lines = content.lines();
    let first = lines.next();
    if first.map(|s| s.trim() != "---").unwrap_or(true) {
        return (String::new(), content.to_string());
    }
    let mut frontmatter = String::new();
    let mut body = String::new();
    let mut in_frontmatter = true;
    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            frontmatter.push_str(line);
            frontmatter.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (frontmatter, body)
}

/// 找到 `## {section}` 章节的起始行号和结束行号（不含下一个 ## 标题）
///
/// v8 D19 修复 ADR-004 v7 已知局限 1 + 实现偏差 D：
/// - 标题行比较改用 `l.trim() == header.trim()`，避免尾部空格导致匹配失败
/// - 同层级同名章节匹配第一个时记录 warn 日志（v7 已知局限 1 落地）
fn find_section_range(body: &str, section: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let header = section.trim();
    let start = lines.iter().position(|l| l.trim() == header)?;
    // 检查是否存在同层级同名章节（已知局限 1：匹配第一个并记录 warn 日志）
    if lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .any(|(_, l)| l.trim() == header)
    {
        warn!(
            section = header,
            body_lines = lines.len(),
            "duplicate section heading, using first match"
        );
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// 在 `## {section}` 范围内找到 `### {subsection}` 的起始行号和结束行号
///
/// v8 D19 新增。subsection 结束行 = 下一个 `###` 或 `##` 或父 section_end。
fn find_subsection_range(body: &str, section: &str, subsection: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let (section_start, section_end) = find_section_range(body, section)?;
    let subsection_header = subsection.trim();
    let start = lines
        .iter()
        .enumerate()
        .skip(section_start + 1)
        .take(section_end.saturating_sub(section_start + 1))
        .find(|(_, l)| l.trim() == subsection_header)
        .map(|(i, _)| i)?;
    // 同层级同名子章节匹配第一个并记录 warn 日志（与 find_section_range 一致）
    if lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .take(section_end.saturating_sub(start + 1))
        .any(|(_, l)| l.trim() == subsection_header)
    {
        warn!(
            section = section.trim(),
            subsection = subsection_header,
            body_lines = lines.len(),
            "duplicate subsection heading, using first match"
        );
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("### ") || l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(section_end);
    Some((start, end))
}

/// apply operations 到 SKILL.md 内容，返回更新后的内容
pub fn apply_skill_operations(
    content: &str,
    operations: &[SkillUpdateOperation],
) -> Result<String, ApplyError> {
    let (frontmatter, body) = split_frontmatter(content);
    let mut frontmatter_lines: Vec<String> = frontmatter.lines().map(|s| s.to_string()).collect();
    let mut body_lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();

    for op in operations {
        match op {
            SkillUpdateOperation::ReplaceSection { section, content } => {
                let range = find_section_range(&body_lines.join("\n"), section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                // 保留 `## {section}` 行，替换后续内容
                body_lines.splice(range.0 + 1..range.1, content.lines().map(|s| s.to_string()));
            }
            SkillUpdateOperation::AddSection {
                after,
                section,
                content,
            } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, after)
                    .ok_or_else(|| ApplyError::SectionNotFound(after.clone()))?;
                let mut new_lines: Vec<String> = vec![section.clone()];
                new_lines.extend(content.lines().map(|s| s.to_string()));
                new_lines.push(String::new()); // 空行分隔
                body_lines.splice(range.1..range.1, new_lines);
            }
            SkillUpdateOperation::RemoveSection { section } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                body_lines.drain(range.0..range.1);
            }
            SkillUpdateOperation::ReplaceFrontmatter { field, value } => {
                if !FRONTMATTER_WHITELIST.contains(&field.as_str()) {
                    return Err(ApplyError::FieldNotWhitelisted(field.clone()));
                }
                let prefix = format!("{}:", field);
                if let Some(line) = frontmatter_lines
                    .iter_mut()
                    .find(|l| l.starts_with(&prefix))
                {
                    *line = format!("{}: {}", field, value);
                } else {
                    frontmatter_lines.push(format!("{}: {}", field, value));
                }
            }
            SkillUpdateOperation::ReplaceSubsection {
                section,
                subsection,
                content,
            } => {
                let body_str = body_lines.join("\n");
                let range =
                    find_subsection_range(&body_str, section, subsection).ok_or_else(|| {
                        ApplyError::SubsectionNotFound(section.clone(), subsection.clone())
                    })?;
                // 保留 `### {subsection}` 标题行，替换后续内容
                body_lines.splice(range.0 + 1..range.1, content.lines().map(|s| s.to_string()));
            }
            SkillUpdateOperation::AddSubsection {
                section,
                after,
                subsection,
                content,
            } => {
                let body_str = body_lines.join("\n");
                let range = find_subsection_range(&body_str, section, after).ok_or_else(|| {
                    ApplyError::SubsectionNotFound(section.clone(), after.clone())
                })?;
                let mut new_lines: Vec<String> = vec![subsection.clone()];
                new_lines.extend(content.lines().map(|s| s.to_string()));
                new_lines.push(String::new()); // 空行分隔
                body_lines.splice(range.1..range.1, new_lines);
            }
            SkillUpdateOperation::RemoveSubsection {
                section,
                subsection,
            } => {
                let body_str = body_lines.join("\n");
                let range =
                    find_subsection_range(&body_str, section, subsection).ok_or_else(|| {
                        ApplyError::SubsectionNotFound(section.clone(), subsection.clone())
                    })?;
                body_lines.drain(range.0..range.1);
            }
            SkillUpdateOperation::ReplaceBody { content } => {
                body_lines = content.lines().map(|s| s.to_string()).collect();
            }
        }
    }

    // v8 D19：post-apply 结构校验，防止 LLM 用 replace_body 或 remove_section
    // 删除所有章节标题，整体回滚（D13 语义）
    let new_body: String = body_lines.join("\n");
    if let Err(e) = validate_skill_structure(&new_body) {
        return Err(ApplyError::StructureInvalid(e.to_string()));
    }

    let mut result = String::new();
    result.push_str("---\n");
    for line in &frontmatter_lines {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("---\n\n");
    for line in &body_lines {
        result.push_str(line);
        result.push('\n');
    }
    Ok(result)
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("section not found: {0}")]
    SectionNotFound(String),
    #[error("subsection not found: {0} / {1}")]
    SubsectionNotFound(String, String),
    #[error("frontmatter field not whitelisted: {0}")]
    FieldNotWhitelisted(String),
    /// v8 D19：post-apply 结构校验失败（apply 后 body 无 `##` 标题或首个 section 空）
    #[error("post-apply structure invalid: {0}")]
    StructureInvalid(String),
}

/// 保留最新 keep 代历史，删除超出部分
pub fn cleanup_skill_history(history_dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(history_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut versions: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // 解析 vN.md
        if let Some(stripped) = file_name.strip_prefix('v')
            && let Some(name) = stripped.strip_suffix(".md")
            && let Ok(v) = name.parse::<u32>()
        {
            versions.push((v, path));
        }
    }

    versions.sort_by_key(|(v, _)| *v);
    let excess = versions.len().saturating_sub(keep);
    for (_, path) in versions.iter().take(excess) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: test\ndescription: A skill\nversion: 1\n---\n\n## Usage\n\nDo it.\n\n## Examples\n\nExample 1.\n";
    const SAMPLE_WITH_SUBSECTIONS: &str = "---\nname: test\ndescription: A skill\n---\n\n## Usage\n\n### Basic\n\nDo step 1.\n\n### Advanced\n\nDo step 2.\n\n## Examples\n\nExample 1.\n";

    #[test]
    fn replace_section_existing() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New usage content.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("New usage content."));
        assert!(!result.contains("Do it."));
    }

    #[test]
    fn replace_section_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Missing".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE, &ops),
            Err(ApplyError::SectionNotFound(_))
        ));
    }

    #[test]
    fn add_section_after_existing() {
        let ops = vec![SkillUpdateOperation::AddSection {
            after: "## Usage".to_string(),
            section: "## Edge Cases".to_string(),
            content: "Edge case notes.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("## Edge Cases"));
        assert!(result.contains("Edge case notes."));
        let usage_idx = result.find("## Usage").unwrap();
        let edge_idx = result.find("## Edge Cases").unwrap();
        let examples_idx = result.find("## Examples").unwrap();
        assert!(usage_idx < edge_idx);
        assert!(edge_idx < examples_idx);
    }

    #[test]
    fn remove_section_existing() {
        let ops = vec![SkillUpdateOperation::RemoveSection {
            section: "## Examples".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(!result.contains("## Examples"));
        assert!(!result.contains("Example 1."));
    }

    #[test]
    fn replace_frontmatter_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "description".to_string(),
            value: "Updated description".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("description: Updated description"));
    }

    #[test]
    fn replace_frontmatter_not_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "version".to_string(),
            value: "999".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE, &ops),
            Err(ApplyError::FieldNotWhitelisted(_))
        ));
    }

    #[test]
    fn replace_frontmatter_upsert_appends_missing_field() {
        // v8 D19：upsert 语义 — 字段不存在则追加
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "self_updatable".to_string(),
            value: "false".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("self_updatable: false"));
    }

    #[test]
    fn replace_subsection_existing() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Advanced".to_string(),
            content: "Do step 2 with caution.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(result.contains("Do step 2 with caution."));
        assert!(!result.contains("Do step 2."));
        // 其他子章节不变
        assert!(result.contains("Do step 1."));
    }

    #[test]
    fn replace_subsection_section_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Missing".to_string(),
            subsection: "### Advanced".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn replace_subsection_subsection_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSubsection {
            section: "## Usage".to_string(),
            subsection: "### Missing".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn add_subsection_after_existing() {
        let ops = vec![SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Basic".to_string(),
            subsection: "### Edge Cases".to_string(),
            content: "Edge case content.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(result.contains("### Edge Cases"));
        assert!(result.contains("Edge case content."));
        let basic_idx = result.find("### Basic").unwrap();
        let edge_idx = result.find("### Edge Cases").unwrap();
        let advanced_idx = result.find("### Advanced").unwrap();
        assert!(basic_idx < edge_idx);
        assert!(edge_idx < advanced_idx);
    }

    #[test]
    fn add_subsection_after_not_found() {
        let ops = vec![SkillUpdateOperation::AddSubsection {
            section: "## Usage".to_string(),
            after: "### Missing".to_string(),
            subsection: "### New".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(
            apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops),
            Err(ApplyError::SubsectionNotFound(_, _))
        ));
    }

    #[test]
    fn remove_subsection_existing() {
        let ops = vec![SkillUpdateOperation::RemoveSubsection {
            section: "## Usage".to_string(),
            subsection: "### Advanced".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE_WITH_SUBSECTIONS, &ops).unwrap();
        assert!(!result.contains("### Advanced"));
        assert!(!result.contains("Do step 2."));
        // 其他子章节不变
        assert!(result.contains("### Basic"));
        assert!(result.contains("Do step 1."));
    }

    #[test]
    fn replace_body_replaces_body_keeps_frontmatter() {
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "## New Section\n\nNew body content.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        // frontmatter 保留
        assert!(result.contains("name: test"));
        assert!(result.contains("description: A skill"));
        // body 被整体替换
        assert!(result.contains("## New Section"));
        assert!(result.contains("New body content."));
        // 原 body 内容消失
        assert!(!result.contains("Do it."));
        assert!(!result.contains("## Usage"));
    }

    #[test]
    fn find_section_range_trailing_whitespace_matches() {
        // v8 D19 修复 ADR-004 v7 实现偏差 D：trim_start → trim，尾部空格也能匹配
        let body = "## Usage \n\nDo it.\n";
        let range = find_section_range(body, "## Usage").unwrap();
        assert_eq!(range.0, 0);
    }

    #[test]
    fn validate_skill_structure_compliant() {
        let body = "## Usage\n\nDo it.\n\n## Examples\n\nExample.\n";
        assert!(validate_skill_structure(body).is_ok());
    }

    #[test]
    fn validate_skill_structure_no_section_heading() {
        let body = "Just plain text without any heading.";
        assert!(matches!(
            validate_skill_structure(body),
            Err(SkillStructureError::NoSectionHeading)
        ));
    }

    #[test]
    fn validate_skill_structure_empty_first_section() {
        // 首个 ## 标题下到下一个 ## 之间无非空内容
        let body = "## First\n\n## Second\n\nContent.\n";
        assert!(matches!(
            validate_skill_structure(body),
            Err(SkillStructureError::EmptyFirstSection)
        ));
    }

    #[test]
    fn apply_replace_body_with_compliant_structure_passes() {
        // v8 D19：replace_body 后 body 仍有 ## 标题，应通过 post-apply 校验
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "## New Section\n\nNew content.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_replace_body_with_no_section_heading_rolls_back() {
        // v8 D19：replace_body 后 body 无 ## 标题，post-apply 校验应返回 StructureInvalid
        let ops = vec![SkillUpdateOperation::ReplaceBody {
            content: "Just plain text without heading.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(matches!(result, Err(ApplyError::StructureInvalid(_))));
    }

    #[test]
    fn apply_remove_all_sections_rolls_back() {
        // v8 D19：删除所有 ## 章节后 body 无 ## 标题，应回滚
        let ops = vec![
            SkillUpdateOperation::RemoveSection {
                section: "## Usage".to_string(),
            },
            SkillUpdateOperation::RemoveSection {
                section: "## Examples".to_string(),
            },
        ];
        let result = apply_skill_operations(SAMPLE, &ops);
        assert!(matches!(result, Err(ApplyError::StructureInvalid(_))));
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cleanup_keeps_latest_3_generations() {
        let tmp = TempDir::new().unwrap();
        let history_dir = tmp.path().join("history");
        fs::create_dir_all(&history_dir).unwrap();
        for v in 1..=6 {
            fs::write(history_dir.join(format!("v{}.md", v)), format!("v{}", v)).unwrap();
        }
        cleanup_skill_history(&history_dir, 3).unwrap();
        let remaining: Vec<_> = fs::read_dir(&history_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 3);
        // 保留最新的 3 代（v4, v5, v6）
        assert!(remaining.contains(&"v4.md".to_string()));
        assert!(remaining.contains(&"v5.md".to_string()));
        assert!(remaining.contains(&"v6.md".to_string()));
    }

    #[test]
    fn cleanup_no_dir_is_noop() {
        let result = cleanup_skill_history(std::path::Path::new("/nonexistent"), 3);
        assert!(result.is_ok());
    }
}

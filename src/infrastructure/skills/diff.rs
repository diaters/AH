use std::path::Path;

use crate::domain::SkillUpdateOperation;

/// 允许 LLM 修改的 frontmatter 字段白名单
pub const FRONTMATTER_WHITELIST: &[&str] = &["name", "description", "self_updatable"];

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
fn find_section_range(body: &str, section: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let header = section.trim();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with(header))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
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
        }
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

#[derive(Debug)]
pub enum ApplyError {
    SectionNotFound(String),
    FieldNotWhitelisted(String),
}

/// 保留最新 keep 代历史，删除超出部分
pub fn cleanup_skill_history(history_dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(history_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 目录不存在视为无操作
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

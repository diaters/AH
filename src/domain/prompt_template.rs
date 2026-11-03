//! Prompt 模板插值
//!
//! 支持变量：`{{kind}}`、`{{body}}`、`{{body_json.<field>}}`
//! 简单字符串替换，非模板引擎。

use anyhow::Result;
use tracing::warn;

/// 校验模板语法。
///
/// 合法变量：`{{kind}}`、`{{body}}`、`{{body_json.<field>}}`
/// `<field>` 必须是非空单层标识符（`[A-Za-z_][A-Za-z0-9_]*`）。
/// 多层路径、空变量、未识别变量均非法。
pub fn validate_template(template: &str) -> Result<()> {
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        // `{{` 之前的字面量文本不允许出现 `}}`
        if rest[..start].contains("}}") {
            anyhow::bail!("stray }} in template");
        }
        let after_start = &rest[start + 2..];
        let Some(end_offset) = after_start.find("}}") else {
            anyhow::bail!("unclosed {{ in template");
        };
        let var = after_start[..end_offset].trim();
        if !is_valid_var(var) {
            anyhow::bail!("invalid template variable: {{{{{var}}}}}");
        }
        rest = &after_start[end_offset + 2..];
    }
    // 最后一个变量之后的剩余文本也不允许出现 `}}`
    if rest.contains("}}") {
        anyhow::bail!("stray }} in template");
    }
    Ok(())
}

fn is_valid_var(var: &str) -> bool {
    if var == "kind" || var == "body" {
        return true;
    }
    if let Some(field) = var.strip_prefix("body_json.") {
        return !field.is_empty()
            && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && field
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_')
                .unwrap_or(false);
    }
    false
}

/// 用 webhook 的 kind 与请求 body 渲染模板。
///
/// 字段不存在时输出空字符串。
pub fn render_template(template: &str, kind: &str, body: &serde_json::Value) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end_offset) = after_start.find("}}") else {
            // 不应发生（已 validate），兜底按字面量输出
            warn!(
                event = "TemplateRenderUnclosedVar",
                template = template,
                "unclosed {{ in template"
            );
            result.push_str("{{");
            rest = after_start;
            break;
        };
        let var = after_start[..end_offset].trim();
        result.push_str(&resolve_var(var, kind, body));
        rest = &after_start[end_offset + 2..];
    }
    result.push_str(rest);
    result
}

fn resolve_var(var: &str, kind: &str, body: &serde_json::Value) -> String {
    match var {
        "kind" => kind.to_string(),
        "body" => body.to_string(),
        other => other
            .strip_prefix("body_json.")
            .and_then(|field| body.get(field))
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_valid_templates() {
        assert!(validate_template("{{kind}}").is_ok());
        assert!(validate_template("{{body}}").is_ok());
        assert!(validate_template("{{body_json.title}}").is_ok());
        assert!(validate_template("{{body_json._user_id}}").is_ok());
        assert!(validate_template("plain text").is_ok());
        assert!(validate_template("{{kind}} - {{body_json.title}}").is_ok());
    }

    #[test]
    fn validate_rejects_invalid_templates() {
        assert!(validate_template("{{body_json.a.b}}").is_err());
        assert!(validate_template("{{body_json.}}").is_err());
        assert!(validate_template("{{foo}}").is_err());
        assert!(validate_template("{{}}").is_err());
        assert!(validate_template("{{body_json.1abc}}").is_err());
    }

    #[test]
    fn validate_rejects_stray_closing_braces_in_literal() {
        assert!(validate_template("foo}}bar").is_err());
    }

    #[test]
    fn validate_rejects_extra_closing_braces_after_var() {
        assert!(validate_template("{{kind}}}}").is_err());
    }

    #[test]
    fn validate_allows_single_closing_brace() {
        assert!(validate_template("}").is_ok());
    }

    #[test]
    fn validate_allows_multiple_single_closing_braces() {
        assert!(validate_template("a}b}c").is_ok());
    }

    #[test]
    fn render_webhook_template() {
        let body = serde_json::json!({"title": "bug", "number": 42});
        assert_eq!(
            render_template(
                "分析 {{kind}}: {{body_json.title}} #{{body_json.number}}",
                "github.issue_opened",
                &body
            ),
            "分析 github.issue_opened: bug #42"
        );
    }

    #[test]
    fn render_missing_field_returns_empty() {
        let body = serde_json::json!({"title": "x"});
        assert_eq!(
            render_template("[{{body_json.missing}}]", "test", &body),
            "[]"
        );
    }
}

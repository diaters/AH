use super::*;
use tempfile::TempDir;

fn write_skill(skills_dir: &Path, dir_name: &str, content: &str) {
    let dir = skills_dir.join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), content).unwrap();
}

fn skill(name: &str, agents: &[&str]) -> LoadedSkill {
    LoadedSkill {
        name: name.to_string(),
        description: format!("{name} desc"),
        instructions: "1. 运行脚本".to_string(),
        dependencies: vec![],
        agents: agents.iter().map(|a| a.to_string()).collect(),
        skill_dir: PathBuf::from(format!(".harness/assets/skills/{name}")),
    }
}

#[test]
fn catalog_lists_name_description_and_dependencies_only() {
    let mut s = skill("smoke-test", &[]);
    s.dependencies = vec!["base".to_string()];
    let prompt = SkillLoader::format_skills_catalog(&[s]);
    assert!(prompt.contains("## 可用技能"));
    assert!(prompt.contains("load_skill"));
    assert!(prompt.contains("- `smoke-test`：smoke-test desc（依赖：base）"));
    assert!(!prompt.contains("1. 运行脚本"), "清单不应包含完整说明");
}

#[test]
fn catalog_empty_returns_empty() {
    assert!(SkillLoader::format_skills_catalog(&[]).is_empty());
}

#[test]
fn skills_without_agents_are_main_agent_only() {
    let s = skill("a", &[]);
    assert!(s.visible_to("main", "main"));
    assert!(!s.visible_to("summarizer", "main"));
    let scoped = skill("b", &["summarizer"]);
    assert!(scoped.visible_to("summarizer", "main"));
    assert!(!scoped.visible_to("main", "main"));
}

#[test]
fn load_all_reads_global_dir_skips_hidden_and_appends_plugins() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    let body = "---\nname: coding\ndescription: coding skill\n---\n\nDo it.\n";
    write_skill(&skills_dir, "coding", body);
    write_skill(
        &skills_dir,
        ".sandbox",
        body.replace("coding", "hidden").as_str(),
    );
    let plugin_md = tmp.path().join("p").join("SKILL.md");
    std::fs::create_dir_all(plugin_md.parent().unwrap()).unwrap();
    std::fs::write(
        &plugin_md,
        "---\nname: negotiation\ndescription: 谈判\n---\n1. 倾听\n",
    )
    .unwrap();
    let plugins = PluginSkillContributions {
        entries: vec![PluginSkillEntry {
            plugin_id: "my-plugin".to_string(),
            skill_id: "negotiation".to_string(),
            path: plugin_md.clone(),
        }],
    };

    let skills = SkillLoader::new(skills_dir).load_all(&plugins);
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["coding", "my-plugin:negotiation"]);
    assert_eq!(skills[1].instructions, "1. 倾听");
    assert_eq!(skills[1].skill_dir, plugin_md.parent().unwrap());
}

#[test]
fn legacy_per_agent_skills_are_reported_not_loaded() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    let legacy = tmp.path().join("agents").join("main").join("skills");
    write_skill(&legacy, "old", "---\nname: old\n---\n");
    let loader = SkillLoader::new(skills_dir);
    assert!(
        loader
            .load_all(&PluginSkillContributions::default())
            .is_empty()
    );
    assert_eq!(loader.legacy_skill_dirs(), vec![legacy.join("old")]);
}

#[test]
fn skill_catalog_finds_by_name() {
    let catalog = SkillCatalog::default();
    catalog.replace(vec![skill("a", &[]), skill("b", &[])]);
    assert_eq!(catalog.find("b").map(|s| s.name), Some("b".to_string()));
    assert!(catalog.find("c").is_none());
}

#[test]
fn parse_reads_agents_and_dependencies_lists() {
    let content = "---\nname: s\ndescription: d\nagents: [summarizer, main]\ndependencies: [ a, , b ]\nversion: 3\n---\n\nBody.\n";
    let parsed = parse_skill_md(content, PathBuf::from("x")).unwrap();
    assert_eq!(parsed.agents, ["summarizer", "main"]);
    assert_eq!(parsed.dependencies, ["a", "b"]);
    assert_eq!(parsed.instructions, "Body.");
}

#[test]
fn parse_malformed_list_falls_back_to_empty() {
    let content = "---\nname: s\ndependencies: browser\nagents: main\n---\n";
    let parsed = parse_skill_md(content, PathBuf::from("x")).unwrap();
    assert!(parsed.dependencies.is_empty());
    assert!(parsed.agents.is_empty());
}

#[test]
fn parse_rejects_unclosed_frontmatter_or_missing_name() {
    assert!(parse_skill_md("---\nname: s\n\nBody\n", PathBuf::from("x")).is_none());
    assert!(parse_skill_md("---\ndescription: d\n---\n", PathBuf::from("x")).is_none());
    assert!(parse_skill_md("no frontmatter\n", PathBuf::from("x")).is_none());
}

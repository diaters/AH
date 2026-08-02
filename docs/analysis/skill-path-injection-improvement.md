# Skill 路径注入改进方案

## 问题

当前 skill 系统在加载和注入 SKILL.md 时存在 __路径盲区__：

1. __LoadedSkill 结构缺失路径字段__
   - 只包含 `name`, `description`, `instructions`, `version`, `self_updatable`
   - 没有 `path` 或 `skill_dir` 字段

2. __parse_skill_md 只解析内容，丢弃路径__
   - 接收 `content: &str`，返回的 LoadedSkill 不包含来源路径

3. __format_skills_prompt 只注入内容，不注入路径__
   - LLM 无法定位 SKILL.md 中引用的相对路径资源（如 `scripts/xxx.sh`）

4. __实际影响__
   - 如果 SKILL.md 提到 `scripts/setup.sh`，LLM 不知道脚本位置
   - 导致无法正确使用 skill 提供的资源

## 改进目标

在注入 SKILL.md 内容时，同时注入 __skill 目录路径__，让 LLM 能够：

1. 知道 SKILL.md 的位置
2. 正确解析相对路径引用
3. 访问 skill 目录下的资源（脚本、配置、模板等）

## 设计方案

### 1. 扩展 LoadedSkill 结构

```rust
pub struct LoadedSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub version: u32,
    pub self_updatable: bool,
    /// ✅ 新增：skill 目录路径（SKILL.md 所在目录）
    pub skill_dir: PathBuf,
}
```

__设计考量__：

- 使用 `skill_dir`（目录路径）而非 `skill_md_path`（文件路径）
- 原因：skill 中的相对路径资源通常是相对于 SKILL.md 所在目录
- 例如：`scripts/setup.sh` → `<skill_dir>/scripts/setup.sh`

### 2. 修改 parse_skill_md 接口

```rust
// 当前签名
pub fn parse_skill_md(content: &str) -> Option<LoadedSkill>

// 改进签名
pub fn parse_skill_md(content: &str, skill_dir: PathBuf) -> Option<LoadedSkill>
```

__变更影响__：

- 需要修改所有调用点，传入 `skill_dir` 参数
- 主要调用点：
  - `SkillLoader::load_skills`：从目录扫描时已有路径
  - `SkillLoader::load_plugin_skills`：从 PluginSkillEntry 获取路径

### 3. 改进 format_skills_prompt 输出

```rust
pub fn format_skills_prompt(skills: &[LoadedSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut prompt = String::from("## 可用技能\n\n");
    for skill in skills {
        prompt.push_str(&format!("### {}\n", skill.name));
        prompt.push_str(&format!("{}\n\n", skill.description));
        // ✅ 新增：注入路径信息
        prompt.push_str(&format!(
            "__Skill 目录__: `{}`\n\n",
            skill.skill_dir.display()
        ));
        prompt.push_str(&format!("{}\n\n", skill.instructions));
    }
    prompt
}
```

__输出示例__：

```markdown
## 可用技能

### my-skill
示例 skill

__Skill 目录__: `.harness/assets/agents/main/skills/my-skill`

使用 scripts/setup.sh 初始化环境。
```

LLM 看到这个 prompt 后，可以正确解析：

- `scripts/setup.sh` → `.harness/assets/agents/main/skills/my-skill/scripts/setup.sh`

### 4. 路径格式选择

有三种路径格式可选：

| 格式 | 示例 | 优点 | 缺点 |
|------|------|------|------|
| __绝对路径__ | `/Users/diater/workspace/Harness/.harness/assets/agents/main/skills/my-skill` | 明确、无歧义 | 泄露系统信息、不可移植 |
| __相对于工作区根目录__ | `.harness/assets/agents/main/skills/my-skill` | 可移植、相对明确 | 需要文档说明基准目录 |
| __相对于 agents 目录__ | `main/skills/my-skill` | 简洁 | 需要知道 agents 目录位置 |

__推荐__：__相对于工作区根目录__ 的相对路径

__理由__：

1. LLM 在工作区根目录下运行，相对路径可以直接使用
2. 不泄露系统绝对路径信息
3. 便于跨环境共享 skill

__实现__：

```rust
impl SkillLoader {
    fn relativize_to_workspace(path: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        path.strip_prefix(&cwd).unwrap_or(path).to_path_buf()
    }
}
```

### 5. 插件 skill 的路径处理

插件 skill 的路径来自 `PluginSkillEntry.path`（SKILL.md 文件路径）：

```rust
pub struct PluginSkillEntry {
    pub plugin_id: String,
    pub skill_id: String,
    pub path: PathBuf, // SKILL.md 文件路径
}
```

需要提取目录路径：

```rust
impl SkillLoader {
    pub fn load_plugin_skills(
        &self,
        contributions: &PluginSkillContributions,
        _agent_name: &str,
    ) -> Vec<LoadedSkill> {
        contributions
            .entries
            .iter()
            .filter_map(|c| {
                let content = std::fs::read_to_string(&c.path).ok()?;
                // ✅ 提取 skill_dir
                let skill_dir = c.path.parent()?.to_path_buf();
                parse_skill_md(&content, skill_dir).map(|mut s| {
                    s.name = format!("{}:{}", c.plugin_id, s.name);
                    s
                })
            })
            .collect()
    }
}
```

## 实施步骤

### Phase 1: 数据结构扩展

1. __LoadedSkill 添加 skill_dir 字段__
   - 文件：`src/infrastructure/skills/loader.rs`
   - 变更：添加 `pub skill_dir: PathBuf` 字段

2. __parse_skill_md 添加路径参数__
   - 文件：`src/infrastructure/skills/loader.rs`
   - 变更：签名改为 `parse_skill_md(content: &str, skill_dir: PathBuf)`

### Phase 2: 调用点适配

1. __SkillLoader::load_skills__
   - 传入扫描到的 skill 目录路径

2. __SkillLoader::load_plugin_skills__
   - 从 `PluginSkillEntry.path` 提取目录路径

3. __SkillLoader::build_registry__
   - 适配新的 parse_skill_md 签名

### Phase 3: Prompt 注入

1. __format_skills_prompt 添加路径注入__
   - 在 description 后、instructions 前注入路径信息

### Phase 4: 测试更新

1. __单元测试适配__
   - 所有测试需要传入 `skill_dir` 参数

2. __集成测试验证__
   - 验证 prompt 输出包含路径信息
   - 验证路径格式正确

## 替代方案

### 方案 A：在 SKILL.md 中声明资源路径

在 SKILL.md 的 frontmatter 中声明资源路径：

```yaml
---
name: my-skill
description: 示例 skill
resource_dir: ./resources  # 相对于 SKILL.md 目录
---
```

__优点__：显式声明，更灵活

__缺点__：增加 skill 编写负担，需要修改 parse_skill_md 逻辑

### 方案 B：使用环境变量注入路径

在运行时注入环境变量：

```rust
prompt.push_str(&format!(
    "Environment: SKILL_DIR={}\n\n",
    skill.skill_dir.display()
));
```

__优点__：通用性强

__缺点__：LLM 需要理解环境变量语义，不如直接路径清晰

## 推荐方案

__推荐主方案__（在 LoadedSkill 中添加 skill_dir 字段），理由：

1. __最小侵入__：只需扩展一个字段和一个参数
2. __语义清晰__：路径作为 skill 元数据的一部分，符合直觉
3. __便于使用__：LLM 可以直接看到路径，无需额外约定
4. __向后兼容__：不影响现有 skill 内容，只是增加路径信息

## 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| 绝对路径泄露系统信息 | 中 | 使用相对路径（相对于工作区根目录） |
| 插件 skill 路径不在工作区内 | 低 | 保持原路径，不做转换 |
| 测试需要大量更新 | 低 | 使用临时目录，更新测试用例 |

## 文档更新

需要更新以下文档：

1. __docs/current-state.md__
   - 更新 skill 系统能力状态

2. __docs/design/skill-system.md__（如果存在）
   - 记录路径注入设计

3. __CLAUDE.md / AGENTS.md__
   - 更新 skill 使用说明

## 后续优化

1. __Skill 资源清单__
   - 在 SKILL.md 中声明可用资源列表
   - 格式：

     ```yaml
     ---
     name: my-skill
     resources:
       - scripts/setup.sh
       - templates/config.yaml
     ---
     ```

2. __Skill 目录结构规范__
   - 定义推荐的 skill 目录结构
   - 例如：

     ```text
     my-skill/
     ├── SKILL.md
     ├── scripts/
     ├── templates/
     └── tests/
     ```

3. __Skill 路径工具函数__
   - 提供 `resolve_skill_resource(skill_name, relative_path)` 工具函数
   - 便于 LLM 和其他系统解析 skill 资源路径

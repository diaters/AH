# ADR-006: Skill Updater 多文件更新支持

## 状态

Proposed（继承 ADR-004 open issue #6，由单独 ADR 推进）

## 背景

ADR-004 建立了 skill-updater 机制，支持通过结构化 diff 操作更新 SKILL.md。当前 8 种操作
（`replace_section` / `add_section` / `remove_section` / `replace_frontmatter` /
`replace_subsection` / `add_subsection` / `remove_subsection` / `replace_body`）全部作用于
SKILL.md 单文件。

但实际 skill 已发展为多文件目录结构：

- __子流程文档__：`download.md` / `analyze.md` / `notify.md` 等 sibling `.md` 文件
- __脚本__：`scripts/*.py` / `scripts/*.sh`
- __模板__：`templates/*.md`

以 `redmine-bug-workflow` skill 为例，目录下包含 1 个 SKILL.md + 7 个子流程 `.md` + 3 个
Python 脚本 + 2 个模板文件。当前 skill-updater 对这些文件完全无感——prompt 中不展示、操作中
不触及。

ADR-004 open issue #6 显式推迟了 `file_refs` 的 updater 支持，建议"单独 ADR 推进
`add_file` / `remove_file` / `replace_file` operation"。本 ADR 即承接此推迟项。

## 决策

### 1. 更新单元模型

__SKILL.md 为主 + sibling 文件为辅__。SKILL.md 仍有特殊地位（frontmatter、版本号、Registry 入口），sibling 文件通过新增的文件级操作管理。

理由：最小改动——现有 8 种 section/subsection 操作完全保留，sibling 文件的管理需求是增量扩展。

### 2. 文件清单感知

__运行时动态扫描__。在 `skill_update_workitem_system` 构造 prompt 时扫描 skill 目录，列出文件树（文件名 + 大小）。文件系统是唯一真相源。

不采用 frontmatter `files:` 声明方式，避免与磁盘状态不一致的维护负担。

### 3. 操作类型扩展

#### 3.1 现有操作加 `path` 字段

7 种 section/subsection/replace_body 操作新增 `path: Option<String>` 字段：

- `path: None`（或 JSON 中省略）→ 作用于 SKILL.md（与现有行为完全一致）
- `path: Some("download.md")` → 作用于 skill 目录下的 `download.md`

`replace_frontmatter` 不加 `path`——只有 SKILL.md 的 frontmatter 由系统管理。

`path` 字段的校验规则：

- 必须是 `.md` 后缀（section 操作的语义前提是 `##` 章节结构）
- 指向的文件必须已存在（新建文件用 `create_file`）
- 路径必须在 skill 目录内（防穿越）

#### 3.2 新增文件级操作

| 操作 | 字段 | 约束 |
|---|---|---|
| `replace_file` | `path, content` | 文件必须已存在；禁止作用于 SKILL.md |
| `create_file` | `path, content` | 文件必须不存在 |
| `delete_file` | `path` | 文件必须已存在；禁止作用于 SKILL.md |

路径校验：在 skill 目录内 + 后缀在白名单内（`.md` / `.py` / `.sh` / `.toml` / `.txt` / `.json`）。

`replace_file("SKILL.md", ...)` 和 `delete_file("SKILL.md")` 作为非法操作被 reject——SKILL.md 的 frontmatter 由系统管理，不允许整文件覆盖。

#### 3.3 新增只读工具

| 工具 | 参数 | 约束 |
|---|---|---|
| `read_skill_file` | `path`（相对于 skill 目录） | 路径沙箱 + 后缀白名单；仅 skill-updater Agent 可用 |

LLM 通过 `read_skill_file` 按需读取 sibling 文件内容，而非在 prompt 中全文注入。理由：

- 避免 token 浪费——多数 update 只涉及 1-2 个文件，不需要全部加载
- LLM 自主决定需要什么上下文，减少无关信息干扰

`read_skill_file` 通过 `required_tag: "skill-updater"` 限制仅 skill-updater Agent 可用。

### 4. 版本语义

__目录级单一版本号__。version 仍在 SKILL.md frontmatter 中，任何文件变更都递增同一个版本号。
与现有语义完全兼容。

### 5. 多轮 WorkItem 协作

skill-updater 的 WorkItem 改为 `multi_turn = true`。LLM 可先调若干次 `read_skill_file` 探查
子文件，最后调 `submit_skill_update` 提交。`submit_skill_update` 调用后视为 WorkItem 完成。

工具过滤从 `["submit_skill_update"]` 扩展为 `["submit_skill_update", "read_skill_file"]`。

### 6. Apply 原子性

__顺序 apply + 失败时目录级快照回滚__。

1. 更新前：将整个 skill 目录复制到 `history/v{current_version}/`（目录级快照）
2. 按操作顺序逐个 apply
3. 任一操作失败 → `cp -r history/v{current_version}/* <skill_dir>/`（整目录回滚）
4. 全部成功 → `set_frontmatter_version` 递增版本号 → 写入新 SKILL.md → 刷新 Registry

与现有单文件备份（`history/v{version}.md`）不同，目录快照保证多文件更新的一致性。

### 7. History/备份

__目录级快照__ `history/v{version}/`，保留最新 3 代。每个快照包含 skill 目录的完整文件树。

现有 `cleanup_skill_history` 逻辑从"删除 `v{N}.md` 单文件"升级为"删除 `v{N}/` 目录"。

### 8. Registry 不扩展

`SkillEntry` 保持不变。sibling 文件信息在 skill update 流程中按需扫描，不持久化到 Registry。
Registry 的职责仍是"SKILL.md 元数据的快速查找"。

## Prompt 构造

````text
## 任务

根据以下经验候选，为现有 skill 提交结构化 diff 更新。

## 原 SKILL.md 完整内容（version N）

```markdown
...
```

## Skill 目录文件树

download.md (2.1KB)
scripts/redmine_download.py (4.3KB)
scripts/notify_reports.py (3.8KB)
templates/triage_report.md (0.5KB)
...

## 经验候选

...

## 要求

1. 使用 read_skill_file 读取需要修改的子文件（仅当需要了解子文件内容时）
2. 调用 submit_skill_update 提交更新
3. 对 SKILL.md：使用 section 级操作（path 省略）
4. 对其他 .md 文件：使用 section 级操作并指定 path（如 "download.md"）
5. 对非 .md 文件：使用 replace_file / create_file / delete_file
6. replace_file / delete_file 不可作用于 SKILL.md
7. path 指定的 .md 文件必须已存在；新建文件使用 create_file
8. section 级操作的 path 只接受 .md 后缀
9. 优先使用颗粒度更细的 operation（subsection > section > replace_body / replace_file）
10. operations 中的 section / subsection 名必须与目标文件中实际存在的标题一致
````

## 后果

### 正面

- skill-updater 能感知和修改 skill 目录下的所有文件，解决多文件 skill 的更新盲区
- `path: Option<String>` 零破坏性——现有 LLM 调用不需要任何修改
- `read_skill_file` 让 LLM 按需获取上下文，避免 token 浪费
- 目录级快照保证多文件更新的原子性

### 负面

- `SkillUpdateOperation` 枚举从 8 种增长到 11 种，LLM 的操作选择空间增大
- 目录级快照比单文件备份占用更多空间（但 skill 目录通常很小，3 代快照在 KB 级别）
- `read_skill_file` 需要多轮 WorkItem 支持，增加了 skill-updater 的执行轮次

### 风险

- __路径穿越__：通过 `validate_skill_file_path` 沙箱校验 + 后缀白名单缓解
- __LLM 误用 replace_file__：通过 prompt 约束 + apply 阶段拒绝 `replace_file("SKILL.md")` 双重防护
- __部分 apply 后回滚延迟__：目录级回滚是 O(文件数) 的复制操作，但 skill 目录通常 < 20 个文件

## 关联文件

- `docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md` — 本 ADR 继承其 open issue #6
- `src/domain/contribution.rs` — `SkillUpdateOperation` 枚举定义
- `src/infrastructure/skills/diff.rs` — `apply_skill_operations` 实现
- `src/systems/experience/skill_update.rs` — skill update 系统
- `src/systems/tools/builtin/submit_skill_update.rs` — skill update 提交工具

---
name: knowledge-management
description: 知识库管理技能,负责检索与维护知识库
---

## 职责
你是知识库管理员,负责知识库的检索与维护。

## 知识库位置
`.harness/knowledge/` 目录

## 知识条目文件格式
每个知识条目为一个 Markdown 文件,包含 YAML frontmatter:

---
status: Approved
source: UserCommand
created_at: 2026-07-10
tags: [shell, usage]
---

# 条目标题

内容...

**元数据字段:**
- `status`:条目状态(`Approved` | `Candidate` | `Deprecated`)
- `source`:来源(`UserCommand` | `BrainReview` | `Agent`)
- `created_at`:创建日期(ISO 8601)
- `tags`:标签数组(用于分类)

## 操作指南

### 检索知识
使用 shell 工具:
- `grep -r "关键词" .harness/knowledge/` - 全文搜索
- `ls -la .harness/knowledge/` - 列出所有条目
- `cat .harness/knowledge/xxx.md` - 读取完整内容

**注意**:检索操作使用 `shell_read`/`shell_list`,无需审批。

### 新增知识
1. 使用 `shell_exec` 创建新文件:
   ```bash
   cat > .harness/knowledge/new-entry.md << 'EOF'
   ---
   status: Approved
   source: Agent
   created_at: 2026-07-10
   tags: [tag1, tag2]
   ---

   # 条目标题

   内容...
   EOF
   ```
2. **需要用户审批**(`shell_exec = Confirm`)

### 更新知识
1. 先使用 `shell_read` 读取现有内容
2. 使用 `shell_exec` 覆盖文件
3. **需要用户审批**

### 删除知识
1. 使用 `shell_exec rm .harness/knowledge/xxx.md`
2. **需要用户审批**

## 响应格式约定

**注意:以下格式为约定而非强约束。调用方应容错解析 LLM 返回的自由文本。**

检索结果建议以结构化格式返回:

{
  "query": "搜索关键词",
  "results": [
    {"title": "条目标题", "file": "xxx.md", "snippet": "摘要..."}
  ],
  "count": 1
}

调用方应:
1. 尝试解析 JSON 格式
2. 若解析失败,直接展示原始文本
3. 不依赖特定字段的存在

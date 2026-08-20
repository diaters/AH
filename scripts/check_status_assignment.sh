#!/usr/bin/env bash
# Task/WorkItem 状态直接赋值断言（P4 领域类型收紧）
#
# 规则（见 docs/design/2026-08-17-complexity-governance-design.md §P4）：
# - Task.status / WorkItem.status 已收窄为 pub(crate)，
#   状态转换一律经 mark_* / assign / start / complete / fail 方法，
#   保证结构化 TaskStatusTransition 日志与幂等保护不被绕过。
# - src/domain/task.rs 与 src/domain/work_item.rs 内的赋值是上述方法的实现，属合法。
#
# 验收（设计文档 §P4）：仓库内不存在对 task.status 的直接赋值。

set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

hits=$(grep -rnE '\.status\s*=\s*(crate::)?(harness::)?(domain::)?(TaskStatus|WorkItemStatus)' \
    src/ tests/ --include='*.rs' 2>/dev/null \
    | grep -v 'src/domain/task.rs' | grep -v 'src/domain/work_item.rs' || true)

if [ -n "$hits" ]; then
    echo "VIOLATION: 发现对 Task/WorkItem status 的直接赋值（应改用 mark_* / 状态转换方法）"
    echo "$hits"
    fail=1
fi

if [ "$fail" -ne 0 ]; then
    echo "状态直接赋值断言失败"
    exit 1
fi

echo "状态直接赋值断言通过"

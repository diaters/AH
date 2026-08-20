#!/usr/bin/env bash
# 模块依赖方向断言（P0 依赖方向治理）
#
# 分层规则（见 docs/design/2026-08-17-complexity-governance-design.md）：
#
# ```text
# domain ← contracts ← {ecs, llm, channels, tui, infrastructure}
#        ← {user_plugins, triggers} ← systems ← plugins ← app
# ```
#
# 规则要点：
# - 下层不允许引用上层（箭头方向为"被依赖"）。
# - 同层括号内不允许相互引用：
#   * {ecs, llm, channels, tui, infrastructure} 组内互禁；
#   * {user_plugins, triggers} 组内互禁。
# - 澄清（设计文档评审补充）：plugins 是 systems 的装配扩展，
#   允许 plugins→systems 单向边；app 位于最顶层，可依赖一切下层。
# - crate::prelude 是 bevy prelude 的中性转发，任何层均可引用。
#
# 验收（设计文档 §4 P0）：src/domain/ 内对上层模块的 grep 必须零命中。

set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

# check <目录> <禁止模块...>
check() {
    local dir="$1"
    shift
    for mod in "$@"; do
        local hits
        hits=$(grep -rn "crate::${mod}::" "src/${dir}" --include='*.rs' 2>/dev/null || true)
        if [ -n "$hits" ]; then
            echo "VIOLATION: src/${dir} 引用了 crate::${mod}::"
            echo "$hits" | head -5
            fail=1
        fi
    done
}

# 第 1 层：domain 只依赖自身
check domain contracts ecs llm channels tui infrastructure user_plugins triggers systems plugins app

# 第 2 层：contracts 依赖 domain
check contracts ecs llm channels tui infrastructure user_plugins triggers systems plugins app

# 第 3 层：{ecs, llm, channels, tui, infrastructure} 依赖 contracts/domain，组内互禁
check ecs llm channels tui infrastructure user_plugins triggers systems plugins app
check llm ecs channels tui infrastructure user_plugins triggers systems plugins app
check channels ecs llm tui infrastructure user_plugins triggers systems plugins app
check tui ecs llm channels infrastructure user_plugins triggers systems plugins app
check infrastructure ecs llm channels tui user_plugins triggers systems plugins app

# 第 4 层：{user_plugins, triggers} 依赖下层，组内互禁
check user_plugins triggers systems plugins app
check triggers user_plugins systems plugins app

# 第 5 层：systems 依赖除 plugins/app 外的一切
check systems plugins app

# 第 6 层：plugins 是 systems 的装配扩展，仅禁止引用 app
check plugins app

# lib.rs 不允许模块级 glob re-export（pub use xxx::*）
if grep -nE "^pub use [a-z_]+::\\*" src/lib.rs | grep -v "^[0-9]+://"; then
    echo "VIOLATION: src/lib.rs 存在模块级 glob re-export"
    fail=1
fi

if [ "$fail" -ne 0 ]; then
    echo "模块依赖方向断言失败"
    exit 1
fi

echo "模块依赖方向断言通过"

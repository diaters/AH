# AI Harness Wiki

## 文档导览

本目录用于记录 `Harness` 项目的代码级 Wiki，覆盖项目概述、架构设计、模块拆解与后续优化方向。

- [项目概述](./01-overview.md)
- [架构设计](./02-architecture.md)
- [应用装配与系统流转](./03-app-and-systems.md)
- [领域模型](./04-domain-model.md)
- [LLM 与前端适配](./05-llm-and-frontend.md)
- [待优化项](./06-optimization.md)

## 阅读顺序

建议按照以下顺序阅读：

1. 先看项目概述，建立整体认知。
2. 再看架构设计，理解分层、资源与主链路。
3. 然后阅读模块详解，进入具体实现。
4. 最后查看待优化项，明确当前代码的演进方向。

## Wiki 地图

```mermaid
flowchart TD
    A[index.md] --> B[01-overview.md]
    A --> C[02-architecture.md]
    A --> D[03-app-and-systems.md]
    A --> E[04-domain-model.md]
    A --> F[05-llm-and-frontend.md]
    A --> G[06-optimization.md]

    C --> D
    C --> E
    C --> F
    D --> G
    E --> G
    F --> G
```

## 代码目录映射

```mermaid
mindmap
  root((src))
    main.rs
    lib.rs
    app
      装配配置
      资源注入
      调度顺序
    domain
      Task 与 Agent
      Memory
      Space
      Frontend 契约
      Evaluation
      Contribution
    systems
      Ingress
      Transform
      Dispatch
      Execution
      Output
      Maintenance
    llm
      Provider
      Factory
      Executor
      Prompt
    tui
      UI 状态
      渲染
      输入处理
```

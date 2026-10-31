//! 经验管理员真实模型场景：检索命中 / 改写命中 / 无关拒绝，以及起草质量的结构检查。
//!
//! 门控：`#[ignore]` + `HARNESS_TEST_REAL_LLM=1`，永不进入 CI。
//! provider 与 Agent 配置与正式运行一致：读取 `HARNESS_PROVIDERS_CONFIG`（默认 `providers.toml`）
//! 与 `HARNESS_AGENTS_CONFIG`（默认 `agents.toml`）中 `experience-librarian` 的提示词与首个模型；
//! 密钥按正式入口的方式从 `.env.local` 读取。
//!
//! ```text
//! HARNESS_TEST_REAL_LLM=1 cargo test --test real_llm_experience_librarian -- --ignored --nocapture
//! ```

use std::time::Duration;

use harness::domain::{
    AgentConfig, AgentExecutionOutput, AgentExecutionRequest, AgentId, AgentRequestKind,
    ExperienceEntry, OutputContent, ProvidersConfig, TaskId, build_draft_prompt,
    build_lookup_prompt, parse_draft, parse_lookup_decision,
};
use harness::llm::ExecutorRegistry;

const TIMEOUT: Duration = Duration::from_secs(180);

struct Librarian {
    registry: ExecutorRegistry,
    provider: String,
    model: String,
    system_prompt: String,
}

fn librarian() -> Option<Librarian> {
    if std::env::var("HARNESS_TEST_REAL_LLM").is_err() {
        eprintln!("SKIP: 需要 HARNESS_TEST_REAL_LLM=1");
        return None;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    dotenvy::from_filename(".env.local").ok();
    let providers_path =
        std::env::var("HARNESS_PROVIDERS_CONFIG").unwrap_or_else(|_| "providers.toml".into());
    let agents_path =
        std::env::var("HARNESS_AGENTS_CONFIG").unwrap_or_else(|_| "agents.toml".into());
    let providers: ProvidersConfig =
        toml::from_str(&std::fs::read_to_string(providers_path).unwrap()).unwrap();
    let agents: AgentConfig =
        toml::from_str(&std::fs::read_to_string(agents_path).unwrap()).unwrap();
    let agent = agents
        .agent
        .into_iter()
        .find(|a| a.tags.iter().any(|t| t == "experience-librarian"))
        .expect("agents config must define experience-librarian");
    let model = agent
        .models
        .first()
        .expect("librarian needs a model")
        .clone();
    Some(Librarian {
        registry: ExecutorRegistry::from_config(&providers).unwrap(),
        provider: model.provider,
        model: model.model,
        system_prompt: agent.system_prompt.expect("librarian system prompt"),
    })
}

impl Librarian {
    async fn ask(&self, prompt: String) -> String {
        let executor = self
            .registry
            .get(&self.provider)
            .expect("provider executor");
        let request = AgentExecutionRequest {
            task_id: TaskId::new(),
            agent_id: AgentId::new(),
            request_kind: AgentRequestKind::Experience,
            prompt,
            system_prompt: Some(self.system_prompt.clone()),
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: Some(self.model.clone()),
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
        };
        let output = tokio::time::timeout(TIMEOUT, executor.execute(request))
            .await
            .expect("librarian timed out")
            .expect("librarian request failed");
        match output {
            AgentExecutionOutput {
                content: OutputContent::Text(text),
                ..
            } => text,
            other => panic!("unexpected librarian output: {other:?}"),
        }
    }
}

fn entry(title: &str, body: &str) -> ExperienceEntry {
    ExperienceEntry::new(title, body, None, chrono::Utc::now()).unwrap()
}

fn library() -> Vec<ExperienceEntry> {
    vec![
        entry(
            "鸿蒙 HAP 安装报 9568322 签名不匹配",
            "## 适用场景\n用 hdc install 安装 HAP 时报 `error: install failed due to grant request permissions failed` 或错误码 9568322，应用装不上真机。\n\n## 可行做法\n用 DevEco 自动签名重新生成调试证书后 `hdc install -r entry-default-signed.hap`。\n\n## 走不通的路线\n- 直接重复安装未签名包：仍报 9568322\n- 卸载旧版本后重装：仍报 9568322，原因是签名证书与设备不匹配\n\n## 前提\nDevEco Studio 5.0，真机已开启开发者模式",
        ),
        entry(
            "macOS 笔记本电源与电池状态监控",
            "## 适用场景\n用户要求监控笔记本是否接通电源、电池电量低于阈值时提醒。\n\n## 可行做法\n`pmset -g batt` 解析输出，配合定时任务每 5 分钟检查。\n\n## 走不通的路线\n- `system_profiler SPPowerDataType`：太慢（数秒），不适合高频轮询\n\n## 前提\nmacOS",
        ),
        entry(
            "SSH 公钥登录提示 Permission denied (publickey)",
            "## 适用场景\nssh 登录服务器报 `Permission denied (publickey)`，已经 ssh-copy-id 过仍失败。\n\n## 可行做法\n服务器上 `chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys`，并确认 home 目录不可被组写。\n\n## 走不通的路线\n- 反复 ssh-copy-id：公钥已在文件里，问题是权限\n\n## 前提\nOpenSSH StrictModes 默认开启",
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要真实模型：HARNESS_TEST_REAL_LLM=1"]
async fn librarian_selects_matching_and_rejects_unrelated() {
    let Some(librarian) = librarian() else { return };
    let entries = library();
    let index: Vec<&ExperienceEntry> = entries.iter().collect();
    let cases: Vec<(&str, Option<usize>)> = vec![
        ("hdc install 安装 HAP 失败，报错 9568322", Some(0)),
        ("鸿蒙应用怎么都装不到手机上，提示和签名有关", Some(0)),
        (
            "ssh 连服务器提示 Permission denied (publickey)，公钥已经拷过去了",
            Some(2),
        ),
        ("帮我查一下最近关于 Rust 异步运行时的技术资料并总结", None),
        ("帮我写一个 Python 脚本统计目录下的文件大小", None),
    ];
    let mut failures = Vec::new();
    for (situation, expected) in cases {
        let reply = librarian.ask(build_lookup_prompt(situation, &index)).await;
        let picked: Vec<String> = parse_lookup_decision(&reply)
            .unwrap_or_else(|e| panic!("unparseable reply for {situation}: {e}\n{reply}"))
            .into_iter()
            .map(|m| m.id)
            .collect();
        let expected_ids: Vec<String> = expected
            .map(|i| vec![entries[i].id.clone()])
            .unwrap_or_default();
        eprintln!("情况：{situation}\n  选择：{picked:?}\n  期望：{expected_ids:?}");
        if picked != expected_ids {
            failures.push(format!(
                "{situation}: picked {picked:?}, expected {expected_ids:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "librarian mismatches:\n{}",
        failures.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要真实模型：HARNESS_TEST_REAL_LLM=1"]
async fn librarian_drafts_failed_routes_and_merges_same_problem() {
    let Some(librarian) = librarian() else { return };
    let entries = library();
    let transcript = "[用户] 帮我把 entry 的 HAP 装到测试机上\n\
        [调用 shell_exec] hdc install entry-default-unsigned.hap\n\
        [失败 shell_exec] error: failed to install bundle. code:9568322 error: signature verification failed\n\
        [Agent] 安装失败，先卸载旧版本再试\n\
        [调用 shell_exec] hdc uninstall com.example.harness && hdc install entry-default-unsigned.hap\n\
        [失败 shell_exec] error: failed to install bundle. code:9568322\n\
        [Agent] 卸载无效，问题在签名。改用 hvigor 带签名配置重新打包\n\
        [调用 shell_exec] hvigorw assembleHap -p buildMode=debug\n\
        [结果 shell_exec] BUILD SUCCESSFUL\n\
        [调用 shell_exec] hdc install -r entry-default-signed.hap\n\
        [结果 shell_exec] install bundle successfully.\n\
        [Agent] 已用签名包安装成功";
    let similar = vec![&entries[0]];
    let reply = librarian
        .ask(build_draft_prompt(
            transcript,
            Some("记下为什么卸载重装没用"),
            &similar,
        ))
        .await;
    let draft = parse_draft(&reply).unwrap_or_else(|e| panic!("unparseable draft: {e}\n{reply}"));
    eprintln!(
        "标题：{}\n合并目标：{:?}\n{}",
        draft.title, draft.merge_target, draft.body
    );
    for section in ["## 适用场景", "## 可行做法", "## 走不通的路线", "## 前提"] {
        assert!(draft.body.contains(section), "missing {section}");
    }
    assert!(
        draft.body.contains("9568322"),
        "error code must be kept for retrieval"
    );
    assert!(
        draft.body.contains("卸载"),
        "failed route from the transcript must be recorded"
    );
    assert_eq!(
        draft.merge_target.as_deref(),
        Some(entries[0].id.as_str()),
        "same problem should merge into the existing entry"
    );
}

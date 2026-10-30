use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::event::{self, Event, KeyEventKind};
use harness::tui::{App, TuiFrontend};
use harness::{
    app::{HeadlessLoop, LoopHooks, build_harness_app, run_loop},
    channels::{Channel, ChannelManager, QqChannel, TelegramChannel},
    domain::{EngineEvent, ExternalInput, Frontend, UserAction},
    llm::ExecutorRegistry,
    systems::{HarnessConfig, HarnessSettings},
};
use tokio::runtime::Runtime;
use tracing::{debug, info, warn};

fn init_tracing() -> (tracing_appender::non_blocking::WorkerGuard, PathBuf) {
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let file_filter = tracing_subscriber::EnvFilter::builder()
        .with_env_var("RUST_LOG")
        .with_default_directive(if cfg!(debug_assertions) {
            "harness=debug".parse().unwrap()
        } else {
            "harness=info".parse().unwrap()
        })
        .from_env_lossy();

    let log_dir = std::env::var("HARNESS_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let process_log = PathBuf::from(&log_dir).join(format!(
        "harness_{timestamp}_{}.processes.jsonl",
        std::process::id()
    ));
    let file_appender =
        tracing_appender::rolling::never(&log_dir, format!("harness_{timestamp}.jsonl"));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(file_filter);

    tracing_subscriber::registry().with(file_layer).init();

    (guard, process_log)
}

/// RAII guard ensuring terminal is restored even on panic.
struct TuiGuard;

impl Drop for TuiGuard {
    fn drop(&mut self) {
        // 禁用启动时启用的终端特性，再恢复 raw mode / alternate screen
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
        );
        ratatui::restore();
    }
}

/// headless 下收到第一个关闭信号后，等待运行时空闲的最长时间。
const HEADLESS_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

fn main() -> Result<()> {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref()
        == Some(harness::systems::tools::backend::supervisor::INTERNAL_FLAG)
    {
        return harness::systems::tools::backend::supervisor::run_internal()
            .map_err(anyhow::Error::msg);
    }
    // `--headless`：不初始化终端，只驱动 ECS；审批只能经 IM 通道完成
    let headless = std::env::args().skip(1).any(|arg| arg == "--headless");

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls ring crypto provider");

    dotenvy::from_filename(".env.local").ok();
    if harness::channels::client::run_admin_command()? {
        return Ok(());
    }
    let (_log_guard, process_log) = init_tracing();
    // 正常入口必须先加载 dotenv 和日志，再让守护器继承宿主环境。
    let process_backend =
        harness::NativeProcessBackend::supervised_current_exe_with_log(&process_log)
            .map_err(anyhow::Error::msg)?;

    // 安装 panic hook：确保 panic 时日志能 flush，并恢复终端
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(
            event = "PanicCaught",
            panic_info = %info,
            "panic occurred in main thread"
        );
        // 兜底：确保 panic 时终端也能被正确恢复（headless 未接管终端）
        if !headless {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture,
                crossterm::event::DisableBracketedPaste,
            );
            ratatui::restore();
        }
        default_hook(info);
    }));

    info!(
        event = "HarnessStarting",
        version = env!("CARGO_PKG_VERSION"),
        build_commit = env!("HARNESS_BUILD_COMMIT"),
        headless,
        process_log = %process_log.display(),
        "AI Harness starting"
    );

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let config = HarnessConfig::from_env()?;
    if headless
        && config.channels.telegram.is_none()
        && config.channels.qq.is_none()
        && config.channels.client.is_none()
    {
        anyhow::bail!("--headless requires at least one channel (HARNESS_CHANNELS_CONFIG)");
    }

    // 启动期配置校验：agents.toml 损坏或重名时立即退出，而非运行期 panic
    harness::infrastructure::incubation::agent_registry::validate_agent_config(
        &config.agents_config_path,
    )?;

    // 加载 providers.toml 或从环境变量构建
    let executor_registry = if std::path::Path::new(&config.providers_config_path).exists() {
        let providers_toml = std::fs::read_to_string(&config.providers_config_path)
            .with_context(|| format!("failed to read {}", config.providers_config_path))?;
        let providers_config: harness::domain::ProvidersConfig =
            toml::from_str(&providers_toml).with_context(|| "failed to parse providers.toml")?;

        info!(
            path = %config.providers_config_path,
            provider_count = providers_config.provider.len(),
            "providers config loaded"
        );

        ExecutorRegistry::from_config(&providers_config)?
    } else {
        info!("providers.toml not found, using global env config as single provider");
        ExecutorRegistry::from_env()?
    };

    // 创建 Frontend channel
    let (event_tx, event_rx) = unbounded::<EngineEvent>();
    let (action_tx, action_rx) = unbounded::<UserAction>();

    // 构建通道列表
    let mut channel_list: Vec<Arc<dyn Channel>> = vec![];
    if let Some(tg_cfg) = config.channels.telegram.clone() {
        info!(
            event = "TelegramChannelEnabled",
            "enabling Telegram channel"
        );
        let config_path = config.channels_config_path.as_ref().map(PathBuf::from);
        channel_list.push(Arc::new(TelegramChannel::new_with_path(
            tg_cfg,
            config_path,
        )));
    }
    if let Some(qq_cfg) = config.channels.qq.clone() {
        info!(event = "QqChannelEnabled", "enabling QQ channel");
        let config_path = config.channels_config_path.as_ref().map(PathBuf::from);
        let workspace_dir = config_path
            .as_ref()
            .and_then(|p| p.parent().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok());
        let mut qq = QqChannel::new_with_path(qq_cfg, config_path);
        if let Some(ws) = workspace_dir {
            qq = qq.with_workspace_dir(ws);
        }
        channel_list.push(Arc::new(qq));
    }

    // 创建 input channel：IM 入向消息和 TUI 输入共用
    let (input_tx, input_rx) = unbounded::<ExternalInput>();

    let client_server = if let Some(client_config) = config.channels.client.clone() {
        let store = harness::channels::client::Store::open(client_config.state_path.clone())?;
        let client = harness::channels::client::Client::new(client_config, store, input_tx.clone());
        let server = runtime.block_on(harness::channels::client::ClientServer::start(
            client.clone(),
        ))?;
        info!(event="ClientChannelEnabled", address=%server.address, "HTTPS client channel enabled");
        channel_list.push(Arc::new(client));
        Some(server)
    } else {
        None
    };

    // 启动 ChannelManager（无通道时为空操作）
    // 必须在 Tokio runtime 上下文中 spawn supervisor 任务。
    let (channel_manager, channel_handle, channel_frontends) = {
        let _guard = runtime.enter();
        ChannelManager::new(channel_list, input_tx.clone())
            .map_err(|e| anyhow::anyhow!("channel configuration error: {e}"))?
    };
    let runtime_clone = runtime.clone();

    // 构建 frontends：TUI（headless 下不注册）+ 所有 IM 通道前端
    let mut frontends: Vec<Box<dyn Frontend>> = Vec::new();
    if !headless {
        frontends.push(Box::new(TuiFrontend::new(event_tx, action_rx)));
    }
    frontends.extend(channel_frontends);

    // 构建 ECS app
    let mut app = build_harness_app(
        config,
        runtime.clone(),
        executor_registry,
        input_rx,
        frontends,
        channel_manager,
    );

    app.insert_resource(process_backend.clone());
    // 在 app / channel / runtime 之前析构，覆盖 TUI 初始化等后续错误返回和 panic unwind。
    let _process_shutdown = ProcessShutdownGuard(process_backend.clone());

    // 加载触发器配置（如果已配置）
    let triggers_config_path = app
        .world()
        .resource::<HarnessSettings>()
        .0
        .triggers_config_path
        .clone();
    let loaded_trigger_config: Option<harness::triggers::TriggerConfig> = if let Some(ref path) =
        triggers_config_path
    {
        use std::path::PathBuf;
        let path = PathBuf::from(path);
        match harness::triggers::load_triggers_config(&path) {
            Ok(trigger_config) => {
                // 校验模板
                if let Err(e) = harness::triggers::validate_templates(&trigger_config) {
                    warn!(
                        event = "TriggersTemplateValidationFailed",
                        error = %e,
                        "trigger template validation failed, skipping triggers"
                    );
                    None
                } else {
                    // 构建 registry
                    match harness::triggers::build_registry_from_config(&trigger_config) {
                        Ok(registry) => {
                            let webhook_count = trigger_config.webhook.routes.len();
                            let timer_count = trigger_config.timer.routes.len();
                            info!(
                                event = "TriggersLoaded",
                                webhook_count, timer_count, "triggers config loaded"
                            );
                            app.world_mut().insert_resource(registry);

                            // 启动 webhook server
                            if trigger_config.webhook.enabled {
                                let input_tx_for_webhook = input_tx.clone();
                                let webhook_config = trigger_config.webhook.clone();
                                let _webhook_guard = runtime.spawn(async move {
                                        if let Err(e) = harness::triggers::run_webhook_server(
                                            input_tx_for_webhook,
                                            webhook_config,
                                        )
                                        .await
                                        {
                                            warn!(event = "WebhookServerError", error = %e, "webhook server exited with error");
                                        }
                                    });
                            }
                            Some(trigger_config)
                        }
                        Err(e) => {
                            warn!(
                                event = "TriggersRegistryBuildFailed",
                                error = %e,
                                "failed to build trigger registry"
                            );
                            None
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    event = "TriggersConfigLoadFailed",
                    error = %e,
                    "failed to load triggers config, continuing without triggers"
                );
                None
            }
        }
    } else {
        None
    };

    // 始终启动 timer scheduler：无论是否配置了 triggers.toml，以支持动态任务模式。
    // 有配置时 SchedulerState.static_routes = Some(...)；无配置时为 None。
    let mut initial_state = harness::triggers::SchedulerState::default();
    if let Some(config) = &loaded_trigger_config {
        initial_state.set_static_routes(harness::triggers::SchedulerRoutes {
            timer: config.timer.clone(),
            webhook: config.webhook.clone(),
        });
    }
    // 恢复持久化的动态定时任务；补跑判定由 Startup 系统完成。
    let schedule_store = harness::infrastructure::schedules::ScheduleStore::default_path();
    // 读取失败（非损坏，如权限问题）时本次会话不启用持久化，避免空状态覆盖磁盘上的定义。
    match schedule_store.load() {
        Ok(definitions) => {
            *initial_state.dynamic_tasks_mut() = definitions;
            app.world_mut().insert_resource(schedule_store);
        }
        Err(e) => warn!(
            event = "ScheduleStoreLoadFailed",
            error = %e,
            "failed to load scheduled tasks, persistence disabled for this session"
        ),
    }
    let (scheduler_tx, scheduler_rx) = tokio::sync::watch::channel(initial_state.clone());
    app.world_mut().insert_resource(initial_state);
    app.world_mut()
        .insert_resource(harness::triggers::SchedulerStateWatcher(Some(scheduler_tx)));

    let input_tx_for_timer = input_tx.clone();
    let _timer_guard = runtime.spawn(async move {
        if let Err(e) =
            harness::triggers::run_timer_scheduler(input_tx_for_timer, scheduler_rx).await
        {
            warn!(event = "TimerSchedulerError", error = %e, "timer scheduler exited with error");
        }
    });

    info!(
        event = "EcsAppBuilt",
        headless, "ECS app built, entering main loop"
    );

    let total_ticks = if headless {
        let signals = Arc::new(AtomicUsize::new(0));
        spawn_shutdown_signal_listener(
            &runtime,
            input_tx.clone(),
            signals.clone(),
            app.world()
                .resource::<harness::contracts::RuntimeWake>()
                .clone(),
        );
        run_loop(
            &mut app,
            &mut HeadlessLoop::new(signals, HEADLESS_SHUTDOWN_GRACE),
        )
    } else {
        // 启用 crossterm 的鼠标和粘贴支持
        crossterm::execute!(
            std::io::stdout(),
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableBracketedPaste,
        )?;
        let _guard = TuiGuard;
        let mut tui = TuiLoop {
            terminal: ratatui::init(),
            app_state: App::new(action_tx),
            event_rx,
            terminal_events: TerminalEvents::start(
                app.world()
                    .resource::<harness::contracts::RuntimeWake>()
                    .clone(),
            ),
            dirty: true,
            last_draw: Instant::now() - Duration::from_millis(16),
        };
        run_loop(&mut app, &mut tui)
    };

    info!(event = "HarnessExiting", total_ticks, "AI Harness exiting");

    process_backend.shutdown().map_err(anyhow::Error::msg)?;

    // 优雅关闭 ChannelManager
    {
        let channel_manager = app.world().resource::<ChannelManager>();
        channel_manager.shutdown();
    }
    let _ = runtime_clone.block_on(channel_handle);
    if let Some(server) = client_server {
        runtime_clone.block_on(server.shutdown())?;
    }

    Ok(())
}

struct ProcessShutdownGuard(harness::NativeProcessBackend);
impl Drop for ProcessShutdownGuard {
    fn drop(&mut self) {
        if let Err(error) = self.0.shutdown() {
            warn!(event = "ProcessSupervisorShutdownFailed", error = %error, "process service cleanup failed");
        }
    }
}

/// 专用输入读取器；退出时最多等待一次 100 ms poll 或队列发送。
struct TerminalEvents {
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl TerminalEvents {
    fn start(wake: harness::contracts::RuntimeWake) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(512);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match event::poll(Duration::from_millis(100)) {
                    Ok(false) => continue,
                    Ok(true) => {}
                    Err(error) => {
                        warn!(event = "CrosstermPollError", %error, "terminal input poll failed");
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
                let mut input = match event::read() {
                    Ok(input) => input,
                    Err(error) => {
                        warn!(event = "CrosstermReadError", %error, "terminal input read failed");
                        continue;
                    }
                };
                loop {
                    match tx.send_timeout(input, Duration::from_millis(100)) {
                        Ok(()) => {
                            wake.notify_ui();
                            break;
                        }
                        Err(crossbeam_channel::SendTimeoutError::Timeout(value)) => {
                            if worker_stop.load(Ordering::Acquire) {
                                return;
                            }
                            input = value;
                        }
                        Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return,
                    }
                }
            }
        });
        Self {
            rx,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for TerminalEvents {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// TUI 运行形态的主循环钩子：读取终端事件、消费引擎事件、渲染界面。
struct TuiLoop {
    terminal: ratatui::DefaultTerminal,
    app_state: App,
    event_rx: Receiver<EngineEvent>,
    terminal_events: TerminalEvents,
    dirty: bool,
    last_draw: Instant,
}

impl LoopHooks for TuiLoop {
    fn poll(&mut self) -> bool {
        let input_count = self.poll_terminal_events();
        let mut engine_event_count = 0u32;
        while let Ok(ev) = self.event_rx.try_recv() {
            debug!(
                event = "EngineEventReceived",
                event_kind = ?ev,
                "received engine event from channel"
            );
            self.app_state.handle_engine_event(ev);
            engine_event_count += 1;
        }
        input_count > 0 || engine_event_count > 0
    }

    fn render(&mut self, active: bool) {
        self.dirty |= active
            || self
                .cleanup_deadline()
                .is_some_and(|at| Instant::now() >= at);
        if self.dirty && self.last_draw.elapsed() >= Duration::from_millis(16) {
            if let Err(e) = self.terminal.draw(|frame| self.app_state.render(frame)) {
                warn!(event = "RenderError", error = %e, "TUI render failed");
            }
            self.dirty = false;
            self.last_draw = Instant::now();
        }
    }
    fn next_deadline(&self) -> Option<Instant> {
        let redraw = self
            .dirty
            .then(|| self.last_draw + Duration::from_millis(16));
        redraw.into_iter().chain(self.cleanup_deadline()).min()
    }

    fn should_quit(&self) -> bool {
        self.app_state.should_quit
    }

    fn heartbeat(&self, tick: u64) {
        debug!(
            event = "TuiLoopHeartbeat",
            tick,
            messages = self.app_state.messages.len(),
            agents = self.app_state.agents.len(),
            tasks = self.app_state.tasks.len(),
            pending_approvals = self.app_state.pending_approvals.len(),
            "TUI main loop heartbeat"
        );
    }
}

impl TuiLoop {
    fn cleanup_deadline(&self) -> Option<Instant> {
        self.app_state
            .tasks
            .iter()
            .filter(|task| task.parent_id.is_none())
            .filter_map(|task| task.completed_at.map(|at| at + Duration::from_millis(5001)))
            .min()
    }

    /// 读取全部待处理的终端事件，返回处理的输入事件数（按键 / 粘贴 / 鼠标）。
    ///
    /// 用非致命方式读取事件，避免 IME 等场景下的错误导致整个程序退出。
    fn poll_terminal_events(&mut self) -> u32 {
        let mut count = 0u32;
        for ev in self.terminal_events.rx.try_iter().take(256) {
            match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    self.app_state.handle_key_event(key);
                    count += 1;
                }
                Event::Key(key) => {
                    // Release / Repeat 事件忽略，避免 IME 组合过程中的误触发
                    debug!(
                        event = "KeyEventIgnored",
                        kind = ?key.kind,
                        code = ?key.code,
                        "ignored non-press key event"
                    );
                }
                Event::Mouse(mouse) => {
                    self.app_state.handle_mouse_event(mouse);
                    count += 1;
                }
                Event::Resize(_, _) => {
                    count += 1;
                }
                Event::Paste(text) => {
                    self.app_state.handle_paste(&text);
                    count += 1;
                }
                _ => {}
            }
        }
        if count > 0 {
            debug!(
                event = "InputProcessed",
                input_count = count,
                mode = ?self.app_state.mode,
                "processed input events"
            );
        }
        count
    }
}

/// headless 下监听 SIGINT / SIGTERM：每个信号计数一次，第一个信号请求优雅关闭。
fn spawn_shutdown_signal_listener(
    runtime: &tokio::runtime::Runtime,
    input_tx: Sender<ExternalInput>,
    signals: Arc<AtomicUsize>,
    wake: harness::contracts::RuntimeWake,
) {
    runtime.spawn(async move {
        #[cfg(unix)]
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!(event = "SigtermHandlerFailed", error = %e, "cannot listen for SIGTERM");
                    None
                }
            };
        loop {
            #[cfg(unix)]
            let received = match terminate.as_mut() {
                Some(term) => tokio::select! {
                    r = tokio::signal::ctrl_c() => r.is_ok(),
                    r = term.recv() => r.is_some(),
                },
                None => tokio::signal::ctrl_c().await.is_ok(),
            };
            #[cfg(not(unix))]
            let received = tokio::signal::ctrl_c().await.is_ok();
            if !received {
                return;
            }
            let count = signals.fetch_add(1, Ordering::SeqCst) + 1;
            wake.notify();
            info!(
                event = "ShutdownSignalReceived",
                count, "shutdown signal received"
            );
            if count == 1 {
                let _ = input_tx.send(ExternalInput::Shutdown);
            }
        }
    });
}

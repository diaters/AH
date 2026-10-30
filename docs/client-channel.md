# 原生客户端接入

## 能力与边界

`client` 是平台无关通道，HarmonyOS 是首个原生客户端。
提供 HTTPS/WSS、一次性配对、设备撤销、可靠同步、任务控制、审批问答、执行观察、附件、定时管理与 MeoW 通知。
输入复用 `ExternalInput`，设备不能指定操作者；审批继续由运行时校验原请求与发起人。
原生工程位于 `clients/harmony`，按窗口宽度在单栏、双栏和三栏之间切换。

全局模型配置、Skills、共享记忆管理与直接进程控制尚未通过客户端接口开放。
进程输出可通过工具结果和后台通知观察，不提供没有运行时依据的暂停/继续按钮。
审批提交的 `queued` 仅表示入队，不能作为运行时最终批准或工具执行成功的证据。
当前仍需完整真机验收，未签名 HAP 不能直接安装；源码或构建通过不代表完整功能验收。

## 服务端配置与启动

保持已有模型和 Agent 配置，在 `HARNESS_CHANNELS_CONFIG` 指向的 TOML 中增加：

```toml
[client]
listen = "0.0.0.0:9443"
instance_name = "我的 Harness"
tls_cert = ".harness/client/server.pem"
tls_key = ".harness/client/server-key.pem"
state_path = ".harness/client/state.json"
```

地址可由局域网或 EasyTier 提供；应用鉴权独立于组网。服务端只接受 TLS。
证书 SAN 必须覆盖客户端实际使用的主机名或 IP，客户端使用系统信任或手动导入的专用 CA。
不要把私钥、配对码、设备令牌或实例状态文件提交到 Git。

下面创建独立的测试 CA 与服务证书；将 `harness.local` 和 IP 换成真实可达地址。
已有可信证书时直接使用现有文件。

```sh
mkdir -p .harness/client
chmod 700 .harness/client
openssl req -x509 -newkey rsa:3072 -nodes \
  -keyout .harness/client/ca-key.pem -out .harness/client/ca.pem \
  -days 365 -subj '/CN=Harness Private CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign'
openssl req -newkey rsa:3072 -nodes \
  -keyout .harness/client/server-key.pem -out .harness/client/server.csr \
  -subj '/CN=harness.local'
cat > .harness/client/server.ext <<'EOF'
subjectAltName=DNS:harness.local,IP:192.168.1.10
basicConstraints=critical,CA:FALSE
extendedKeyUsage=serverAuth
keyUsage=critical,digitalSignature,keyEncipherment
EOF
openssl x509 -req -in .harness/client/server.csr \
  -CA .harness/client/ca.pem -CAkey .harness/client/ca-key.pem -CAcreateserial \
  -out .harness/client/server.pem -days 90 -extfile .harness/client/server.ext
chmod 600 .harness/client/*key.pem
```

在服务未运行时，生成服务端指定用户的一次性配对码，再启动服务：

```sh
export HARNESS_CHANNELS_CONFIG=channels.toml
cargo run -- --client-pair alice
cargo run -- --headless
```

管理命令输出实例 ID、配对码及 300 秒有效期。每个新设备单独生成一枚码。
状态持有跨进程独占锁，禁止与运行实例并发修改；当前新增配对需安排一次重启。
配对码只使用一次，服务端只保存码与令牌的 SHA-256 摘要。设备令牌有效期 90 天。
两台设备使用同一用户配对即可访问相同会话，设备 ID、用户 ID、会话 ID 相互独立。

客户端填写 HTTPS 地址、实例 ID、配对码；使用私有 CA 时粘贴 `ca.pem` 的全文。
先核对实例身份，再发送设备凭证。不要粘贴私钥。
令牌保存在 HarmonyOS Asset Store，地址、CA、草稿及待发送命令保存在应用沙箱。

## 协议与恢复

| 接口 | 用途 |
| --- | --- |
| `GET /v1/identity` | 不带令牌核对公开的实例 ID、名称和协议版本 |
| `POST /v1/pair` | 使用实例 ID、配对码和设备名称取得独立凭证 |
| `GET /v1/capabilities` | 返回实际可用的操作、保留范围与恢复能力 |
| `GET /v1/diagnostics` | 当前用户的待办、重启未知命令、存储状态与同步边界，不返回配置密钥 |
| `GET /v1/devices` | 查询当前用户的设备，不返回令牌摘要 |
| `DELETE /v1/devices/{id}` | 撤销当前用户的设备，活动 WSS 连接随之失效 |
| `GET /v1/snapshot` | 在同一把锁内取得当前用户快照与一致游标 |
| `GET /v1/events?after=N` | 按游标读取当前用户的增量事件 |
| `GET /v1/stream?after=N` | WSS 补发与订阅，使用 Authorization 头鉴权 |
| `POST /v1/commands` | 提交结构化命令，同一 ID 与同一内容复用原回执 |
| `GET /v1/commands/{id}` | 查询原命令状态 |
| `PUT /v1/sessions/{session}/files/{id}?name=文件名` | 二进制上传；相同 ID、名称和内容可安全重试 |
| `GET /v1/files/{id}` | 当前用户的文件下载，服务端先校验长度与 SHA-256 |
| `POST /v1/notifications/{id}/ack` | 客户端主动确认同步或已读 |
| `POST /v1/notifications/{id}/retry` | 失败或未知投递的显式重试 |

除公开身份和配对接口外，均使用 `Authorization: Bearer <token>`。
普通请求体上限 64 KiB，附件上传单独限 10 MiB，图片限 5 MiB；用户文本上限 32 KiB；消息开头的 `/` 指令暂不接受，避免绕过结构化控制协议。
客户端取快照后用该游标订阅，快照与订阅之间的事件从持久化历史补发。
全实例保留最近 2048 条事件，每会话快照保留最近 500 条消息，截断有显式标记。
游标超出保留范围或指向未来时返回 `snapshot_required`，需要重新取快照。
WSS 写入超过 10 秒则断开；客户端用原游标补发或重新同步，不能静默跳过事件。

命令字段：

| `operation` | 必需字段（均包含 `command_id` UUID） |
| --- | --- |
| `create_session` | `title` |
| `send_text` | `session_id`、`content`；可选 `attachment_ids`（最多 8 个） |
| `stop_task` / `finish_task` | `session_id`、`task_id`、`expected_revision` |
| `answer_question` | `session_id`、`task_id`、`question_id`、`answer` |
| `schedule` | `session_id`、`action`（见下文） |
| `respond_approval` | `session_id`、`request_id`、`expected_version`、`option_id`；可选 `feedback` |

审批的版本与选项来自最新快照，同一请求只有一次入队机会，运行时仍须再校验。
停止/结束绑定原 Task 和版本；回答绑定原 `ask_user` 工具调用 ID，不从普通聊天猜测请求。
控制与定时命令经 ECS 处理后，回执更新为 `processed` 或 `rejected`，失败码在 `result_code` 中；
`processed` 表示控制处理完成，不等于任务成功。普通文本和审批的入队回执仍不宣称最终消费。

文件只能通过当前用户、当前会话的附件 ID 进入输入；直接提交服务器路径标记会被拒绝。
上传只保存文件，点击“发送”才交给 Agent。网络中断时客户端保留同一 ID 与本地缓存重试；
服务端可恢复数据落盘后、元数据提交前的同内容上传，不覆盖内容不同的孤立文件。
出向 `channel_send` 可注册已授权的本地文件产物，不抓取任意远程 URL。
实例最多保存 256 个文件，尚无自动清理；需要保留客户端状态与 `client-files` 目录的配套备份。
工具开始/结果/权限审计每会话保留 200 条，单条工具输出最多 12800 字符，截断有显式标记。
这不是全部进程 stdout 的实时流，不把工具返回、任务终态和迟到返回混为一谈。
回执 `server_persisted` 不表示设备收到或用户已读；`queued` 不表示 Agent 已消费。
命令先写入账本再进入统一输入队列；网络失败后应查询或重试同一 ID。
崩溃窗口内的命令重启后标为 `unknown_after_restart`，不会自动重放，需要核对实际结果。
ECS 任务目前不跨进程恢复：保留客户端历史，但旧运行态标为 `recovery_required`，旧审批失效。
状态文件损坏或存储写失败会明确报错并停止接纳新命令，不静默重建身份库。

实例上限为 256 个设备、256 个会话、10000 条命令回执；达到上限返回 `capacity_reached`。
去重账本没有自动淘汰，不能通过清空记录来宣称旧命令仍可安全重试。
后续需补充归档、在线配对管理、日志背压和更大规模存储。

## 定时管理

`operation: "schedule"` 的 `action.kind` 支持 `list`、`create`、`update`、`delete`、`history`。
创建携带 `input`；修改携带 `schedule_id`、`expected_revision` 与 `input`；删除绑定相同版本；
历史查询携带 `schedule_id`，返回最近 50 条真实运行记录。
`input` 包含 `name`、`content`、`schedule`、`enabled`、`notify`（`reply` 或 `explicit`）。
规则使用现有 `cron:0 8 * * *` 或 `once:2026-11-01T08:00:00+08:00` 语法。

只管理当前用户在 Client 通道的动态计划，复用 ScheduleStore、统一修改入口与 scheduler 唤醒。
创建时所选会话是结果目的地；跨会话修改不改变目的地。规则使用服务端本地时区，界面显示其偏移。
删除或一次性计划移除后，已知计划归档仍可查询授权运行账本。
重新启动后的缓存标为过期，先刷新；执行结果、通道接受回执和 MeoW 接受记录分别呈现。

## MeoW 通知

在上述 `[client]` 后添加（`alice` 与配对用户一致）：

```toml
[client.meow]
daily_limit = 3000
utc_offset_minutes = 480

[client.meow.recipients]
alice = "${HARNESS_MEOW_RECIPIENT}"
```

将接收昵称放在服务端环境变量 `HARNESS_MEOW_RECIPIENT`，不放进客户端或通用代码。
不配置该段时关闭外部通知。配置校验失败则拒绝启动，不静默降级。
遵照用户的会员额度，不套用非会员频率限制；仍处理真实 429、网络失败与每日额度。
接口依据 [MeoW 官方文档](https://www.chuckfang.com/MeoW/api_doc.html)，同时检查 HTTP 与 JSON 业务状态。

审批、待回答、根任务完成/失败及 `channel_send` 后台结果会在同一状态事务内写入发件箱。
只投递通用摘要和会话标识，不发送工具参数、详细正文或凭据。
现有定时/监控的 Channel 回执表示实例持久化接受；MeoW worker 将外部投递结果另外落账。
任务不等待用户打开客户端。前台由 WSS 同步，后台不依赖自研应用常驻或 Push Kit。

- `queued` / `sending`：待投递 / 请求进行中。
- `accepted`：MeoW 接口接受，不表示设备送达。
- `failed`：明确拒绝或可安全重试的失败已达自动尝试上限。
- `unknown`：超时、模糊响应或发送中进程退出，可能已发出，不自动重发。
- `quota_wait`：额度用完，等待下个日界线。
- `received_at` / `read_at`：仅由鉴权客户端主动确认，MeoW 没有送达/已读回调。

日界线按固定偏移 `utc_offset_minutes`（默认 UTC+08:00）计算，
每个接收昵称在本实例每天最多预留 3000 次尝试，可配置更低；未知结果也保守计数。
多个发送实例或其他程序使用同一昵称时不会共享额度，需在部署侧统筹。
连接失败或明确限流最多自动尝试 3 次；未知结果必须显式确认可能重复后重试，总尝试上限 20。
发件箱上限 4000 条，只淘汰已读且终结的记录；无法入队时累加明确的溢出计数，不阻塞任务。
客户端显示最近 200 条并标注截断，支持查看原会话、标记已读和显式重试。
尚未验证 MeoW 点击直接唤起应用，当前通知提示手动打开工作台同步。

## 构建与验证

使用匹配的真实 HarmonyOS API 26 SDK 和 hvigor，不修改 SDK 元数据伪装版本：

```sh
cd clients/harmony
hvigorw --mode module -p product=default -p module=entry@default assembleHap --no-daemon
```

工程不携带签名凭据，在 DevEco 中配置自己的签名后安装到授权设备。
未签名产物位于 `entry/build/default/outputs/default/entry-default-unsigned.hap`。

真机签名材料必须匹配 `com.diater.harness`：证书公钥与本机 CSR/私钥一致，
调试 Profile 的包名、设备绑定和有效期正确。SDK 公共 OpenHarmony 测试签名即使通过
`verify-app`，也不等于目标手机信任该签名；应使用开发者平台签发的调试证书及 Profile。
私钥和口令保留本机，部署材料放在工程外或被忽略的 `.harness/` 中，不提交到 Git。

使用匹配签名后的 HAP 安装与启动，所有设备命令指定目标：

```sh
hdc -t <TARGET> install <SIGNED_HAP>
hdc -t <TARGET> shell aa start -a EntryAbility -b com.diater.harness
```

```sh
cargo test --lib channels::client
cargo test --test client_runtime_flow
```

前者验证 TLS、身份隔离、幂等、撤销、补发、审批问答竞争、附件与发件箱；
MeoW 使用本地 mock，覆盖事件自动触发、HTTP/业务失败、超时、重启与额度。
后者经过真实 HTTPS 和现有 ECS，使用 mock provider 验证输入、回复、结束控制和定时管理落盘。
另有 `systems::interaction`、`systems::client_schedules` 和工具结果系统的边界测试。
`tests/fixtures/client-tls` 中的证书及私钥仅供本地自动化测试，禁止用于部署。

HOP-AL10 / API 26 已实测签名安装、TLS 配对、真实模型回复、审批问答、停止及结束任务、
前后台与进程重启后的凭证/草稿恢复、文件保存后重新选择上传、图片预览，以及定时创建/编辑/启停/删除。
原生 HTTP 禁用响应缓存和连接复用；自定义 CA 使用证书摘要区分文件路径，
已实测正确 CA 连接后在同一进程切换到不可信 CA 会立即拒绝连接。
真机完整验收仍需检查撤销、多设备竞争、锁屏、网络切换与恢复，
以及手机/平板/折叠设备的分屏和宽度变化。后台不依赖常驻 WSS，回到前台重新同步。
单台设备上的结果不代表其他设备、折叠布局和全部生命周期场景已验收。
真实定时任务已触发 MeoW 投递，服务端记录 HTTP 与业务状态成功，用户已确认手机收到通知；
回到应用已同步后台任务结果。锁屏和不同网络下的通知表现仍需独立验证。

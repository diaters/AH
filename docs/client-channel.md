# 原生客户端接入

## 能力与边界

`client` 是平台无关通道，HarmonyOS 是首个原生客户端。
当前提供 HTTPS/WSS、一次性配对、独立设备凭证、撤销、会话创建、消息入队、任务状态与审批。
输入复用 `ExternalInput`，设备不能指定操作者；审批继续由运行时校验原请求与发起人。
原生工程位于 `clients/harmony`，按窗口宽度在单栏、双栏和三栏之间切换。

尚未提供结构化停止/结束、按问题 ID 回答、附件上传下载、工具完成日志、自动化管理、MeoW 自动通知与管理诊断。
这些能力不在协议的 `operations` 中，客户端也不提供对应按钮。
审批提交的 `queued` 仅表示入队，不能作为运行时最终批准或工具执行成功的证据。
当前仍需真机验证，未签名 HAP 不能直接安装；源码或构建通过不代表完整功能验收。

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
| `GET /v1/devices` | 查询当前用户的设备，不返回令牌摘要 |
| `DELETE /v1/devices/{id}` | 撤销当前用户的设备，活动 WSS 连接随之失效 |
| `GET /v1/snapshot` | 在同一把锁内取得当前用户快照与一致游标 |
| `GET /v1/events?after=N` | 按游标读取当前用户的增量事件 |
| `GET /v1/stream?after=N` | WSS 补发与订阅，使用 Authorization 头鉴权 |
| `POST /v1/commands` | 提交结构化命令，同一 ID 与同一内容复用原回执 |
| `GET /v1/commands/{id}` | 查询原命令状态 |

除公开身份和配对接口外，均使用 `Authorization: Bearer <token>`。
请求体上限 64 KiB；用户文本上限 32 KiB；消息开头的 `/` 指令暂不接受，避免绕过结构化控制协议。
客户端取快照后用该游标订阅，快照与订阅之间的事件从持久化历史补发。
全实例保留最近 2048 条事件，每会话快照保留最近 500 条消息，截断有显式标记。
游标超出保留范围或指向未来时返回 `snapshot_required`，需要重新取快照。
WSS 写入超过 10 秒则断开；客户端用原游标补发或重新同步，不能静默跳过事件。

命令字段：

| `operation` | 必需字段（均包含 `command_id` UUID） |
| --- | --- |
| `create_session` | `title` |
| `send_text` | `session_id`、`content` |
| `respond_approval` | `session_id`、`request_id`、`expected_version`、`option_id`；可选 `feedback` |

审批的版本与选项来自最新快照，同一请求只有一次入队机会，运行时仍须再校验。
回执 `server_persisted` 不表示设备收到或用户已读；`queued` 不表示 Agent 已消费。
命令先写入账本再进入统一输入队列；网络失败后应查询或重试同一 ID。
崩溃窗口内的命令重启后标为 `unknown_after_restart`，不会自动重放，需要核对实际结果。
ECS 任务目前不跨进程恢复：保留客户端历史，但旧运行态标为 `recovery_required`，旧审批失效。
状态文件损坏或存储写失败会明确报错并停止接纳新命令，不静默重建身份库。

实例上限为 256 个设备、256 个会话、10000 条命令回执；达到上限返回 `capacity_reached`。
去重账本没有自动淘汰，不能通过清空记录来宣称旧命令仍可安全重试。
后续需补充归档、在线配对管理、日志背压和更大规模存储。

## 构建与验证

使用匹配的真实 HarmonyOS API 26 SDK 和 hvigor，不修改 SDK 元数据伪装版本：

```sh
cd clients/harmony
hvigorw --mode module -p product=default -p module=entry@default assembleHap --no-daemon
```

工程不携带签名凭据，在 DevEco 中配置自己的签名后安装到授权设备。
未签名产物位于 `entry/build/default/outputs/default/entry-default-unsigned.hap`。

```sh
cargo test --lib channels::client
cargo test --test client_runtime_flow
```

前者验证 TLS、配对、身份隔离、幂等、撤销、补发与审批竞争；
后者经过真实 HTTPS 和现有 ECS，使用 mock provider 验证输入至回复的完整链路。
`tests/fixtures/client-tls` 中的证书及私钥仅供本地自动化测试，禁止用于部署。

真机仍需检查配对、证书错误、撤销、多设备审批、断线重连、服务重启、草稿恢复，
以及手机/平板/折叠设备的分屏和宽度变化。后台不依赖常驻 WSS，回到前台重新同步。
MeoW 尚未接入，不承诺应用退到后台后的任务通知。

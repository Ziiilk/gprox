# gprox

Rust CLI，将本机已登录 ChatGPT 的 Codex CLI 接到带 Bearer Key 的 OpenAI 兼容 HTTP 接口。

```text
OpenAI SDK / 客户端 → gprox HTTP → codex app-server（stdio JSON-RPC）→ ChatGPT 订阅
```

需要安装 Rust 和 Codex CLI，并已执行 `codex login`。本项目使用现有登录，由 Codex 管理凭据刷新；不会读取或向调用方返回 Codex 的 OAuth Token。调用消耗现有订阅额度，代理 Key 只对本代理有效。

## 安装与启动

```powershell
cargo install --path . --locked

gprox start                 # 前台运行；Ctrl+C 退出
gprox stop                  # 关闭当前代理，前台/后台均适用

gprox service start         # 后台运行，关闭终端后继续工作
gprox service stop          # 关闭后台代理
gprox status                # 查看地址和进程状态
gprox service status        # 同上
gprox key                   # 显示本地代理 API Key
```

安装后的程序在 `%USERPROFILE%\.cargo\bin\gprox.exe`。若终端尚未更新 PATH，可使用这个完整路径，或直接使用构建后的 `target\release\gprox.exe`。

默认地址：`http://127.0.0.1:8787/v1`。首次运行会生成随机 API Key。`start`/`service start` 的配置参数会保存，后续启动继续使用。

```powershell
gprox service start --host 0.0.0.0 --port 8787 --timeout 300 --max-concurrency 2
# 其他设备填写 http://这台电脑的局域网IP:8787/v1，以及 gprox key 的输出
```

`service` 管理的是独立后台进程，不注册 Windows 系统服务，也不设置开机自启。默认仅本机访问；监听 `0.0.0.0` 后是否可从其他设备访问还取决于系统防火墙。公网部署应在前面配置 HTTPS 反向代理，避免在公网明文传输 Key。

Windows 会自动定位 npm 安装中的原生 `codex.exe`，不通过 shell 包装器执行。特殊安装位置可指定：

```powershell
gprox start --codex 'C:\path\to\codex.exe'
```

状态默认存放在 `~/.gprox/`，可用 `--home <目录>` 或 `GPROX_HOME` 覆盖。使用自定义目录时，`start`、`stop`、`status`、`key` 应使用同一个目录。

| 文件 | 用途 |
| --- | --- |
| `config.json` | 监听地址、超时、并发数、Codex 路径及代理 Key |
| `runtime.json` | 当前实例和本机管理接口凭据，停止后删除 |
| `service.log` | 后台启动与退出日志，不记录请求正文或 Key |
| `workspace/` | Codex 临时会话工作目录 |

修改 Key：停止代理，编辑 `config.json` 中的 `api_key`（至少 32 个无空白 ASCII 字符），再启动。妥善保护状态目录；Windows 继承目录 ACL，Unix 目录权限为 `0700`，凭据文件为 `0600`。

## OpenAI SDK 示例

先用 `gprox key` 获取 Key，再使用客户端自己的配置或环境变量传入。

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8787/v1",
    api_key=os.environ["GPROX_API_KEY"],
)

models = client.models.list()
model = models.data[0].id  # 也可以从返回列表中选择具体模型

answer = client.chat.completions.create(
    model=model,
    messages=[{"role": "user", "content": "只回答：你好"}],
)
print(answer.choices[0].message.content)

for chunk in client.chat.completions.create(
    model=model,
    messages=[{"role": "user", "content": "写一句简短问候"}],
    stream=True,
    stream_options={"include_usage": True},
):
    if chunk.choices:
        print(chunk.choices[0].delta.content or "", end="", flush=True)

response = client.responses.create(model=model, input="只回答：你好", store=False)
print(response.output_text)
```

PowerShell：

```powershell
$headers = @{ Authorization = "Bearer $(gprox key)" }
$models = Invoke-RestMethod http://127.0.0.1:8787/v1/models -Headers $headers
$body = @{
    model = $models.data[0].id
    messages = @(@{ role = 'user'; content = 'Reply with exactly OK' })
} | ConvertTo-Json -Depth 8
Invoke-RestMethod http://127.0.0.1:8787/v1/chat/completions `
    -Method Post -Headers $headers -ContentType 'application/json' -Body $body
```

## 兼容范围

| 接口 | 支持 |
| --- | --- |
| `GET /health` | 带 Key 的存活检查 |
| `GET /v1/models` | 启动时从当前 Codex 账户读取的模型列表 |
| `POST /v1/chat/completions` | 文本多轮历史、`system`/`developer` 指令、普通 JSON、SSE、`reasoning_effort`、流式 usage |
| `POST /v1/responses` | 文本 `input` 或消息数组、`instructions`、普通 JSON、SSE 生命周期事件、`reasoning.effort` |

这是文本接口适配，并非完整 OpenAI API。每个 HTTP 请求创建独立、临时 Codex 会话。消息历史转换为带角色的 JSON 文本传给 Codex；`system`/`developer` 内容合并为 Codex 的开发者指令，因此不能保证与原生 OpenAI 消息语义完全相同。

目前不支持函数/工具调用、图片/音频/文件、结构化输出、`temperature`、`top_p`、`max_tokens`/`max_output_tokens`、多候选，以及 `previous_response_id` 或服务端历史存储。不支持的非空参数返回 `400`，不会静默忽略。Responses 默认不存储，只接受 `store=false`。`metadata`、`user` 可传入但不转发。`n` 只接受 `1`。

Token 统计直接使用 Codex 上报数值（可能含其内部提示消耗），不会估算或虚构；未上报时 `usage=null`。模型是否可调用和可用推理档位仍由订阅决定。重启代理可刷新模型列表。

每个请求独立启动一个 App Server，有进程启动开销。默认并发数 2，超出返回 `429`；超时默认 300 秒，普通请求返回 `504`。流开始后的错误使用 SSE 错误事件返回；客户端断开、超时或代理关闭时会取消后端工作。Windows 通过 Job Object 清理 Codex 子进程树。

代理为文本用途关闭 Codex shell、执行、浏览器、插件、应用、MCP 等能力，设置只读沙箱并拒绝交互审批，不暴露远程命令执行接口。管理端口只绑定本机，使用与 API Key 不同的随机凭据；客户端 API Key 无法调用停止接口。

## 开发与验证

```powershell
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release --locked
```

自动化测试使用独立编译的模拟 Codex，无需登录或消耗订阅，覆盖认证、前后台启动/停止、重复启动、普通与流式响应、Unicode、usage、参数拒绝、后端失败、超时、并发上限和断连取消。真实 Codex 接入需要本机登录后验证。

真实验收（需要 Node 18+，会进行 4 次短生成，消耗少量订阅额度）：

```powershell
gprox service start
node scripts/smoke.mjs
```

可通过 `GPROX_BIN`、`GPROX_HOME`、`GPROX_BASE_URL`、`GPROX_MODEL` 调整验收目标。默认模型 `gpt-5.5` 已在本机验证；模型列表是 Codex 目录，列出不代表当前订阅一定有调用权限。`GPROX_DEBUG=1` 启动时会在日志记录协议方法名，方便定位，不记录提示内容。

协议参考：[Codex App Server](https://learn.chatgpt.com/docs/app-server)、[Codex 认证](https://learn.chatgpt.com/docs/auth)。App Server 协议随 Codex 版本变化；本实现针对本机 Codex CLI `0.149.0` 验证。

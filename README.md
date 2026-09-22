# gprox

Rust CLI，将本机已登录 ChatGPT 的 Codex CLI 接到带 Bearer Key 的 OpenAI 兼容 HTTP 接口。

```text
OpenAI SDK / 客户端 → gprox HTTP → codex app-server（stdio JSON-RPC）→ ChatGPT 订阅
```

需要安装 Codex CLI，并已执行 `codex login`；从源码安装还需要 Rust，下载 Release 程序无需 Rust。本项目使用现有登录，由 Codex 管理凭据刷新；不会读取或向调用方返回 Codex 的 OAuth Token。调用消耗现有订阅额度，代理 Key 只对本代理有效。

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
gprox version --json        # 机器可读版本信息
gprox update --check        # 只检查最新正式版本
gprox update                # 下载、验证并替换当前可执行文件
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

## 更新与发布

发布与自动更新支持 **Windows x64 MSVC**。

```powershell
gprox --version
gprox version --json
gprox update --check
gprox update
```

`update` 查询 `Ziiilk/gprox` 最新正式 GitHub Release，只升级到更高的稳定版本，不安装预发布版或降级。默认匿名访问 GitHub API；收到 403/429 时，才临时调用 `gh auth token --hostname github.com` 重试，不保存或打印 GitHub Token。未发布任何 Release 时，会明确提示并正常退出。

更新先下载 Windows ZIP 和 `SHA256SUMS`，校验 ZIP 哈希、解压并核对程序的 `--version`，然后替换正在执行的 `gprox` 文件。不会更新源码目录。网络、哈希或版本校验失败时，现有程序和代理继续保留。这里的 SHA-256 校验用于检测下载损坏及打包错误，并非独立的发布者签名。

若当前 `--home` 对应的代理正在运行，安装时会短暂停止，并通过原安装路径恢复为后台进程；活动请求会被取消。替换失败时也会尝试恢复代理。原来未启动的代理保持停止。其他 `--home` 实例不自动重启；配置、代理 Key 和 Codex 登录保持不变。`--check` 不下载、不停止代理，也不创建本地状态目录。

版本约定：

- `Cargo.toml` 的 `[package].version` 是唯一版本来源，格式为 `MAJOR.MINOR.PATCH`；`Cargo.lock` 中的 gprox 版本必须同步。
- CLI 的 `--version`、`version --json` 直接读取编译时 Cargo 版本，不维护额外常量。
- 日常修复递增 patch；功能阶段升级递增 minor 并归零 patch；重大不兼容升级递增 major 并归零 minor/patch。当前 `0.x` 阶段不承诺跨 minor 兼容。
- 发布标签使用轻量 tag `vX.Y.Z`，必须与 Cargo 两处版本完全一致；不覆盖已有 tag。

维护者操作（PowerShell 7）：

```powershell
# 默认 patch；同时修改 Cargo.toml 与 Cargo.lock，预览不写文件
pwsh -File scripts/bump-version.ps1 -Part patch -DryRun
pwsh -File scripts/bump-version.ps1 -Part patch
# 也支持 -Part minor、-Part major，或 -Version 0.2.0

pwsh -File scripts/check-version.ps1
cargo fmt --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings

# 本地验证发布包；ZIP 根目录只包含 gprox.exe、LICENSE、README.md
pwsh -File scripts/package-release.ps1

# 提交本轮发布变更后：检查、创建 tag、推送 tag
git add Cargo.toml Cargo.lock
git commit -m 'chore: 更新发布版本'
pwsh -File scripts/tag-release.ps1 -DryRun
pwsh -File scripts/tag-release.ps1 -Push
```

发版前应将本轮全部代码和工作流一并提交；`tag-release.ps1` 要求工作区干净。首次发版可直接使用当前尚未发布的 `0.1.0`，无须先递增。脚本默认只创建本地 tag；只有显式传入 `-Push` 才会推送到 `origin`。若先创建了本地 tag，之后可运行 `git push origin refs/tags/vX.Y.Z`。

推送 `v*` tag 会触发 `.github/workflows/release.yml`：验证 tag/Cargo 版本 → 格式、测试、Clippy → Windows release 构建 → ZIP 与 SHA-256 清单 → GitHub Release 和自动生成的说明。发布使用 GitHub Actions 自带的 `GITHUB_TOKEN`，无需将个人 Token 放入仓库。打包脚本会映射编译时的用户目录和项目绝对路径，避免程序中的错误位置带出本机路径。打包产物位于被 Git 忽略的 `dist/`：

```text
gprox-x86_64-pc-windows-msvc.zip
SHA256SUMS
```

也可从 [Releases](https://github.com/Ziiilk/gprox/releases) 手动下载；manifest 包含与这些附件名称一致的 cargo-binstall 元数据。仓库目前未配置 crates.io 发布，源码安装继续使用 `cargo install --git https://github.com/Ziiilk/gprox --locked`。

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

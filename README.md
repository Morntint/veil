# Veil — 轻量化企业级 HTTP API 网关

> 基于 Rust 语言开发的**轻量化、高性能、低内存、高安全**企业级 HTTP API 网关。
> 单二进制部署、无 GC 内存开销、异步全栈非阻塞、线程安全原生保障，
> 适配微服务集群、边缘计算、小型云原生集群、内部 API 统一收口场景。

## 核心特性

| 能力 | 说明 |
| --- | --- |
| 高性能网络 | HTTP/1.1、TCP 连接池、请求复用、超时管控、优雅启停 |
| 异步并发 | Tokio 多线程调度、无锁任务分发、背压机制 |
| 配置管理 | TOML 多环境叠加、`VEIL_*` 环境变量覆盖、文件监听热更新、校验兜底回滚 |
| 路由代理 | 精确/前缀/正则匹配、加权轮询/随机/最小连接/平滑加权轮询、反向代理 |
| 安全防护 | Token 鉴权、IP 黑名单、令牌桶限流、请求大小限制、CORS 跨域 |
| 容错重试 | 5xx + 幂等方法重试、连接错误重试、超时重试、每次重试重新选上游 |
| 路径改写 | 正则捕获组替换、保留 query string、路由级配置 |
| 监控可观测 | Prometheus 指标、链路追踪 span、结构化日志、健康检查 |
| 运维管理 | 配置查询、状态查询、路由列表、手动重载 API |

## 技术栈

- **Runtime**: Tokio 1.40（full features）
- **HTTP**: Hyper 1.4 + hyper-util 0.1 + http-body 1
- **Web 框架**: axum 0.7（with macros）
- **中间件**: tower 0.4 + tower-http 0.5
- **配置**: serde + toml 0.8 + notify 6（热更新）
- **日志**: tracing + tracing-subscriber（JSON / text）
- **指标**: prometheus 0.13
- **限流**: governor 0.6（GCRA 令牌桶）
- **并发原语**: parking_lot 0.12 + once_cell 1
- **错误**: thiserror + anyhow

## 目录结构

```
veil/
├── Cargo.toml
├── config/                    # 配置文件目录
│   ├── default.toml           # 默认基础配置
│   ├── dev.toml               # 开发环境覆盖
│   └── prod.toml              # 生产环境覆盖
└── src/
    ├── main.rs                # 程序入口
    ├── lib.rs                 # 模块统一导出
    ├── constant/              # 全局常量
    ├── config/                # 配置加载/校验/热更新/结构体
    ├── core/                  # 路由匹配/负载均衡/反向代理/请求上下文
    ├── network/               # 服务监听/连接池/HTTP 协议处理
    ├── middleware/            # CORS/鉴权/限流/IP黑名单/重试/改写
    ├── monitor/               # 指标/追踪/日志/健康检查
    ├── admin/                 # 运维管理 API + 可视化 Dashboard
    └── utils/                 # 错误类型/时间/线程安全工具
```

## 快速开始

### 环境要求

- Rust 1.75+（推荐 1.96）
- cargo 1.96+

### 编译与运行

```bash
# 编译
cargo build

# 运行（默认使用 config/default.toml）
cargo run

# 指定环境
$env:VEIL_ENV="dev"; cargo run        # PowerShell
VEIL_ENV=dev cargo run                # Bash

# 发布构建（LTO + strip，产物极小）
cargo build --release
./target/release/veil
```

### 运行验证

启动后默认监听 `0.0.0.0:8080`，可用以下端点验证：

```bash
# 根路径
curl http://127.0.0.1:8080/
# => veil is running

# 健康检查（JSON）
curl http://127.0.0.1:8080/health
# => {"config_version":1,"env":"default","routes_count":1,"status":"ok",...}

# 可视化 Dashboard（浏览器打开）
# http://127.0.0.1:8080/_admin/dashboard

# 运维状态
curl http://127.0.0.1:8080/_admin/status

# 路由列表
curl http://127.0.0.1:8080/_admin/routes

# 当前生效配置
curl http://127.0.0.1:8080/_admin/config

# 手动触发配置重载
curl -X POST http://127.0.0.1:8080/_admin/reload
# => {"config_version":2,"message":"配置重载成功","success":true}

# Prometheus 指标
curl http://127.0.0.1:8080/metrics

# 代理转发（需先启动上游服务，否则返回 502）
curl http://127.0.0.1:8080/api/hello
```

### 单元测试

```bash
cargo test
# 运行 58 个测试，全部通过
```

## 配置说明

### 多环境叠加

加载顺序（后者覆盖前者）：

1. `config/default.toml` — 基础配置
2. `config/{env}.toml` — 环境覆盖（`env` 由 `VEIL_ENV` 指定，默认 `default`）
3. 环境变量 — 优先级最高

### 支持的环境变量

| 变量名 | 说明 | 默认值 |
| --- | --- | --- |
| `VEIL_ENV` | 运行环境 | `default` |
| `VEIL_CONFIG_DIR` | 配置文件目录 | `config` |
| `VEIL_HOST` | 监听地址 | `0.0.0.0` |
| `VEIL_SERVER_PORT` | 监听端口 | `8080` |
| `VEIL_LOG_LEVEL` | 日志级别 | `info` |
| `VEIL_LOG_FORMAT` | 日志格式（`json` / `text`） | `json` |

### 配置文件示例

```toml
# config/default.toml
[server]
host = "0.0.0.0"
port = 8080
graceful_shutdown_timeout_secs = 30

[network]
read_timeout_secs = 30
request_size_limit_bytes = 1048576   # 1MB

[proxy]
timeout_secs = 30
max_idle_per_host = 50

[[routes]]
name = "example-service"
match = { type = "prefix", path = "/api" }
upstream = ["http://127.0.0.1:9001", "http://127.0.0.1:9002"]
load_balance = "round_robin"
retries = 1

[security]
enable_rate_limit = true
rate_limit_per_second = 1000
rate_limit_burst = 2000
enable_ip_blacklist = false

[cors]
enable = false

[auth]
enable = false
token = ""
header_name = "authorization"
scheme = "Bearer"
skip_paths = ["/health", "/_admin"]

[monitor]
enable_metrics = true
metrics_path = "/metrics"
enable_health_check = true
health_path = "/health"

[admin]
enable = true
prefix = "/_admin"
```

### 路由匹配类型

| 类型 | 说明 | 示例 |
| --- | --- | --- |
| `exact` | 精确匹配 | `/api/users` 仅匹配该路径 |
| `prefix` | 前缀匹配 | `/api` 匹配 `/api`、`/api/users`、`/api/v1/...` |
| `regex` | 正则匹配 | `^/api/v[0-9]+/` |

### 负载均衡策略

| 策略 | 说明 |
| --- | --- |
| `round_robin` | 轮询（默认） |
| `random` | 随机选取 |
| `least_conn` | 最小活跃连接数（自动故障剔除） |
| `weighted_round_robin` | 平滑加权轮询（nginx 算法，配合 `upstream_weights`） |

### 路径改写

路由级配置，支持正则捕获组：

```toml
[[routes]]
name = "legacy-api"
match = { type = "prefix", path = "/old" }
upstream = ["http://127.0.0.1:9001"]
[routes.rewrite]
enable = true
path_pattern = "/old/(.+)"
path_replace = "/new/$1"
```

## API 端点

### 业务端点

| 路径 | 方法 | 说明 |
| --- | --- | --- |
| `/` | GET | 根路径，返回运行状态字符串 |
| `/{path}` | * | 反向代理至上游（按路由配置匹配） |

### 监控端点

| 路径 | 方法 | 说明 |
| --- | --- | --- |
| `/health` | GET | 健康检查（JSON：status/version/uptime/config_version/routes_count） |
| `/metrics` | GET | Prometheus 指标（text/plain） |

### 运维端点（前缀 `/_admin`）

| 路径 | 方法 | 说明 |
| --- | --- | --- |
| `/_admin/dashboard` | GET | 可视化 Dashboard（HTML 实时监控面板） |
| `/_admin/config` | GET | 当前生效配置（含版本号） |
| `/_admin/status` | GET | 网关运行状态 |
| `/_admin/routes` | GET | 路由列表摘要 |
| `/_admin/reload` | POST | 手动触发配置重载（兜底回滚） |

> 运维端点默认跳过鉴权（通过 `auth.skip_paths` 配置）。

## Prometheus 指标

| 指标 | 类型 | 标签 | 说明 |
| --- | --- | --- | --- |
| `gateway_http_requests_total` | counter | method, status, route | HTTP 请求总数 |
| `gateway_http_request_duration_seconds` | histogram | method, route | HTTP 请求处理耗时 |
| `gateway_active_connections` | gauge | - | 当前活跃连接数 |
| `gateway_rate_limit_total` | counter | ip | 限流触发次数 |
| `gateway_auth_failures_total` | counter | reason | 鉴权失败次数 |
| `gateway_upstream_requests_total` | counter | upstream, status | 上游转发总数 |
| `gateway_upstream_request_duration_seconds` | histogram | upstream | 上游转发耗时 |
| `gateway_upstream_retries_total` | counter | route | 上游重试次数 |

Histogram 桶：`0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10` 秒。

## 中间件链路

请求处理顺序（外 → 内）：

```
请求 → CORS → 限流 → IP黑名单 → 鉴权 → Body限制 → 超时 → 代理转发 → 响应
```

- 外层中间件先执行，尽早拦截非法请求
- 各中间件独立开关，实时读取 `SharedConfig`，配置热更新立即生效
- 限流器参数变化时自动重建（per-IP 状态重置）

## 错误响应

所有错误返回统一 JSON 结构：

```json
{
  "error": "错误描述",
  "code": 502
}
```

| 错误类型 | HTTP 状态码 |
| --- | --- |
| `Auth` | 401 Unauthorized |
| `RateLimit` | 429 Too Many Requests |
| `PayloadTooLarge` | 413 Payload Too Large |
| `Timeout` | 504 Gateway Timeout |
| `Route` | 404 Not Found |
| `Validation` | 400 Bad Request |
| `Proxy` / `Network` | 502 Bad Gateway |
| 其他 | 500 Internal Server Error |

## 热更新

- **自动监听**：`notify` 监听配置目录变更，去抖 300ms 后重载
- **校验兜底**：重载失败或校验不通过时保留旧配置，避免宕机
- **版本自增**：每次成功热更新 `config_version` +1
- **手动触发**：`POST /_admin/reload`
- **实时生效**：路由、限流、鉴权、CORS 等配置对新请求立即生效

## 优雅关闭

- 监听 `Ctrl+C`（Windows/Linux）和 `SIGTERM`（Linux）
- 收到信号后停止接受新连接，等待 in-flight 请求完成
- axum 0.7 内置 drain 机制

## 架构分层

```
┌─────────────────────────────────────────┐
│ 对外服务层  /_admin  /metrics  /health  │
├─────────────────────────────────────────┤
│ 监控运维层  指标 追踪 日志 健康检查       │
├─────────────────────────────────────────┤
│ 插件中间件层  CORS 鉴权 限流 IP黑名单 ...│
├─────────────────────────────────────────┤
│ 网关核心层  路由 负载均衡 反向代理 上下文 │
├─────────────────────────────────────────┤
│ 网络传输层  TCP监听 连接池 协议解析       │
├─────────────────────────────────────────┤
│ 底层基础层  配置 日志 错误 工具 常量      │
└─────────────────────────────────────────┘
```

## 性能优化

Release 构建配置：

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

- LTO 跨模块内联优化
- 单 codegen-unit 最大化优化空间
- strip 去除符号信息，减小二进制体积
- `panic = abort` 避免 unwind 开销

## License

MIT OR Apache-2.0

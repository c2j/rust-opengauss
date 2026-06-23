# 贡献指南

感谢您对 `rust-opengauss` 的关注！本文档涵盖了有效贡献所需的全部信息。

## 目录

- [行为准则](#行为准则)
- [开发环境搭建](#开发环境搭建)
- [项目结构](#项目结构)
- [构建与测试](#构建与测试)
- [代码风格](#代码风格)
- [Pull Request 流程](#pull-request-流程)
- [提交规范](#提交规范)
- [使用 Docker 测试](#使用-docker-测试)
- [发布流程](#发布流程)

---

## 行为准则

互相尊重，保持建设性和协作精神。在所有互动中假定善意。

---

## 开发环境搭建

### 前置条件

- **Rust** 1.85+（通过 [rustup](https://rustup.rs/) 安装）
- **Docker**（可选，用于 PostgreSQL 集成测试）
- **Git**

### 克隆与设置

```sh
git clone https://github.com/c2j/rust-opengauss.git
cd rust-opengauss
```

### 安装 Rust 工具链

```sh
rustup default stable
rustup component add rustfmt clippy
```

---

## 项目结构

```
rust-opengauss/
├── crates/
│   ├── gaussdb/                 # 公共 facade crate（重新导出异步/同步客户端）
│   │   └── src/
│   │       └── lib.rs          # 根目录暴露异步 API；同步 API 通过 `sync` feature 启用
│   │
│   ├── opengauss/              # 同步客户端库
│   │   └── src/
│   │       ├── client.rs       # 同步客户端 API
│   │       ├── connection.rs   # 同步连接管理
│   │       ├── transaction.rs  # 事务支持
│   │       ├── binary_copy.rs  # 二进制 COPY 协议
│   │       ├── copy_in_writer.rs
│   │       ├── copy_out_reader.rs
│   │       ├── row_iter.rs     # 行迭代器
│   │       └── notifications.rs # LISTEN/NOTIFY 支持
│   │
│   ├── tokio-opengauss/         # 异步客户端库（基于 tokio）
│   │   └── src/
│   │       ├── client.rs       # 异步客户端 API
│   │       ├── connection.rs   # 异步连接管理
│   │       ├── connect.rs      # 连接建立
│   │       ├── connect_socket.rs # TCP Socket 连接
│   │       ├── connect_tls.rs  # TLS 连接
│   │       ├── query.rs        # 查询执行
│   │       ├── prepare.rs      # 预编译语句
│   │       ├── statement.rs    # 语句类型
│   │       ├── portal.rs       # Portal（游标）支持
│   │       ├── transaction.rs  # 事务支持
│   │       ├── copy_in.rs      # COPY FROM
│   │       ├── copy_out.rs     # COPY TO
│   │       ├── cancel_query.rs # 查询取消
│   │       ├── tls.rs          # TLS 抽象
│   │       ├── types.rs        # 类型映射
│   │       ├── socket.rs       # Socket 抽象
│   │       ├── keepalive.rs    # TCP keepalive
│   │       └── error/          # 错误类型和 SQLSTATE 映射
│   │
│   ├── opengauss-derive/        # 过程宏 crate: #[derive(ToSql, FromSql)]
│   │   └── src/
│   │       └── lib.rs
│   │
│   ├── opengauss-protocol/      # 线协议 v3.0+ 实现
│   │   └── src/
│   │       ├── message/        # 协议消息类型（后端、前端）
│   │       └── authentication/ # 认证消息处理（MD5、SCRAM、SHA256）
│   │
│   ├── opengauss-types/         # 类型系统：ToSql / FromSql trait、类型映射
│   │   └── src/
│   │       ├── lib.rs
│   │       └── type_gen.rs     # 生成的类型定义
│   │
│   ├── opengauss-native-tls/    # tokio-opengauss 的 native-tls 连接器
│   │   └── src/
│   │       └── lib.rs
│   │
│   └── opengauss-openssl/       # tokio-opengauss 的 openssl 连接器
│       └── src/
│           └── lib.rs
│
├── tools/
│   ├── gaussdb-mcp/             # MCP 服务器 + CLI 工具
│   │   └── src/
│   │       ├── main.rs         # 入口、日志、诊断、CLI/MCP 调度
│   │       ├── server.rs       # MCP 工具实现、SQLSTATE 映射、错误处理
│   │       ├── config.rs       # 配置解析、密钥链、URL 解析
│   │       ├── cli.rs          # CLI 模式实现（SQL 执行器）
│   │       ├── connection.rs   # TLS 检测、连接建立
│   │       ├── output.rs       # 输出格式化（表格、JSON）
│   │       └── queries.rs      # 内省查询的 SQL 模板
│   │
│   └── codegen/                 # 代码生成工具
│       └── src/
│           └── main.rs
│
├── tests/                       # 集成测试
│   └── opengauss-derive-test/
│
├── docker/
│   ├── docker-compose.yml      # PostgreSQL 测试环境
│   └── sql_setup.sh            # 测试用户和扩展设置
│
├── lib/
│   └── opengauss/              # 旧版（workspace 之前）目录
│
├── license/                    # 许可证文件
│   ├── LICENSE-APACHE
│   └── LICENSE-MIT
│
├── Cargo.toml                  # 工作区定义
├── Cross.toml                  # 交叉编译配置
└── rustfmt.toml                # 格式化配置
```

`gaussdb` 是唯一的公共 library crate。`crates/` 下的其他所有 crate 均为 `publish = false` 的内部工作区成员。

### Crate 依赖关系图

```
外部使用者
  └─ gaussdb（公共 facade）
      ├─ tokio-opengauss（异步客户端，publish = false 内部 crate）
      │   └─ opengauss-types（类型系统，publish = false 内部 crate）
      │       └─ opengauss-protocol（线协议，publish = false 内部 crate）
      ├─ opengauss（同步客户端，publish = false 内部 crate）
      │   └─ tokio-opengauss（通过内部封装共享）
      ├─ opengauss-native-tls（native-tls 连接器，publish = false 内部 crate）
      │   └─ native-tls
      └─ opengauss-openssl（openssl 连接器，publish = false 内部 crate）
          └─ openssl

gaussdb-mcp（工具）
  └─ gaussdb（公共 facade）

opengauss-derive（过程宏，publish = false 内部 crate）
  └─ 独立，重新导出 ToSql/FromSql 派生宏

codegen（开发工具）
  └─ 独立，生成类型映射代码
```

---

## 构建与测试

### 构建

```sh
# 构建全部
cargo build

# 仅构建 MCP 工具
cargo build -p gaussdb-mcp

# Release 构建
cargo build --release
```

### 运行测试

```sh
# 运行所有测试（需要运行中的 PostgreSQL 实例）
cargo test

# 运行特定 crate 的测试
cargo test -p tokio-opengauss
cargo test -p opengauss

# 运行测试并显示输出
cargo test -- --nocapture
```

### 代码检查与格式化

```sh
# 检查格式化
cargo fmt --check

# 应用格式化
cargo fmt

# 运行 clippy
cargo clippy -- -D warnings

# 针对特定 crate 运行 clippy
cargo clippy -p gaussdb-mcp -- -D warnings
```

### 构建验证

提交 PR 前请确认：

```sh
cargo fmt --check         # 无格式问题
cargo clippy -- -D warnings  # 无 clippy 警告
cargo build --release     # Release 构建成功
cargo test               # 所有测试通过
```

---

## 代码风格

### Rust

我们遵循由 `rustfmt` 和 `clippy` 强制执行的 Rust 标准约定。

**rustfmt.toml：**
```toml
edition = "2024"
```

### 关键约定

1. **错误处理**：使用结构化错误类型。MCP 工具将 SQLSTATE 映射到 SQLCODE 以提供丰富的错误上下文。
2. **异步**：MCP 工具和 `tokio-opengauss` 是异步的（tokio）；MCP 工具通过 `gaussdb` facade 访问 `tokio-opengauss`。同步 `opengauss` crate 封装异步客户端。
3. **配置**：使用 serde + TOML 进行配置。敏感值（密码）通过操作系统密钥链处理。
4. **日志**：使用 `tracing` crate。日志写入文件而非 stderr，避免干扰 MCP stdio 传输。
5. **无 `unsafe`** 代码，除非有充分理由并经过彻底审查。

### MCP 工具约定

- `server.rs` 中的工具实现使用 `rmcp` 的 `#[tool]` 宏
- 所有工具通过 `CallToolResult::success()` 返回结构化 JSON
- 错误返回 `McpError::internal_error()` 并附带详细的 JSON `data` 负载
- 外部使用者通过 `gaussdb::Client::query(sql, &[])` 参数化 SQL 查询；内部 crate 仍使用 `tokio_opengauss::Client::query(sql, &[])`
- 连接状态管理使用 `Arc<Mutex<HashMap<String, ConnectionState>>>`

### 库约定

- 公共 API 通过 `gaussdb` 暴露，它重新导出原始 `tokio-postgres` crate 的 tokio-opengauss/opengauss 约定
- 类型系统使用 `ToSql` 和 `FromSql` trait
- 功能标志控制可选的类型支持（chrono、uuid、serde_json 等）

---

## Pull Request 流程

### 开始之前

1. **搜索已有的 Issues** — 确认您的想法/问题是否已在讨论中
2. **重大功能或变更先开 Issue** — 获取早期反馈
3. **讨论实现方案** — 在投入大量时间之前确认方向

### 创建 PR

1. Fork 仓库
2. 创建功能分支：`git checkout -b feat/my-feature`
3. 按照上述代码风格进行修改
4. 为新功能添加测试
5. 确保所有检查通过：`cargo fmt --check && cargo clippy && cargo build && cargo test`
6. 推送并创建 Pull Request

### PR 检查清单

- [ ] 代码遵循 `rustfmt` 格式化（`cargo fmt --check`）
- [ ] 无 clippy 警告（`cargo clippy -- -D warnings`）
- [ ] 构建成功（`cargo build --release`）
- [ ] 测试通过（`cargo test`）
- [ ] 新功能包含测试
- [ ] 文档已更新（README、相关 docs/）
- [ ] 提交信息遵循约定（见下文）

### PR 审查

- 所有 PR 在合并前至少需要一次审查
- CI 必须通过（构建、测试、clippy、fmt）
- 保持 PR 聚焦 — 每个 PR 一个功能/修复
- 及时响应审查反馈

---

## 提交规范

我们使用约定式提交，前缀如下：

| 前缀 | 用途 |
|--------|---------|
| `feat:` | 新功能 |
| `fix:` | Bug 修复 |
| `chore:` | 维护、版本升级、依赖更新 |
| `docs:` | 文档更改 |
| `style:` | 格式化、空格、分号 |
| `refactor:` | 代码重构（无行为变化） |
| `test:` | 添加或修改测试 |
| `perf:` | 性能优化 |

**作用域**（可选，但多 crate 变更时建议使用）：

```
feat(gaussdb-mcp): 添加 cli 子命令用于 SQL 执行
fix(protocol): 处理 PG18 中可变长度的 BackendKeyData
chore(gaussdb-mcp): 版本升级至 0.2.3
```

---

## 使用 Docker 测试

提供了 Docker Compose 设置用于 PostgreSQL 集成测试：

```sh
# 启动 PostgreSQL 测试实例
docker compose up -d

# 此实例配置如下：
# - 端口: 5433
# - 用户: pass_user、md5_user、scram_user、ssl_user（密码均为 'password'）
# - TLS 证书（自签名）
# - 扩展: hstore、citext、ltree
# - pg_hba.conf: 用于测试的认证方式

# 运行测试
cargo test

# 完成后停止
docker compose down
```

### 测试用户凭据

| 用户 | 密码 | 认证方式 |
|------|----------|-------------|
| `postgres` | `postgres` | trust |
| `pass_user` | `password` | password（明文） |
| `md5_user` | `password` | md5 |
| `scram_user` | `password` | scram-sha-256 |
| `ssl_user` | （无） | trust（要求 SSL） |

---

## 发布流程

发布由维护者管理。流程如下：

1. 更新 `tools/gaussdb-mcp/Cargo.toml` 中的版本号
2. 更新更新日志（如果存在）
3. 创建版本升级提交：`chore(gaussdb-mcp): bump to x.y.z`
4. 打标签：`git tag gaussdb-mcp-vx.y.z`
5. 推送标签：`git push --tags`

Crate 版本遵循语义化版本：
- `opengauss` / `tokio-opengauss`：遵循上游（tokio-postgres）版本
- `gaussdb`：0.x.y 独立语义化版本，与 tokio-opengauss 耦合（tokio-opengauss 发生破坏性变更时，gaussdb 也进行破坏性版本升级）
- `gaussdb-mcp`：独立的语义化版本（当前为 0.5.0）；标签使用 `gaussdb-mcp-v<version>`

---

## 获取帮助

- **Issues**：在 [GitHub Issues](https://github.com/c2j/rust-opengauss/issues) 提交 Bug 或功能请求
- **Discussions**：在 [GitHub Discussions](https://github.com/c2j/rust-opengauss/discussions) 提问
- **文档**：查看 [docs/](docs/) 获取详细指南

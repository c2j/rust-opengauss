# cli 子命令 -i/--interactive 交互式 SQL REPL

> **For implementer:** REQUIRED SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task.

## Goal

为 `gaussdb-mcp cli` 子命令新增 `-i/--interactive` 标志，启动一个 readline 风格的交互式 SQL REPL（类似 psql）。用户可在终端中编辑多行 SQL、浏览历史、用点命令切换输出目标。

## Reference Spec (用户提供)

- 主提示符 `$ `，续行提示符 `> `
- 以 `;` 结尾才执行（跨多行）
- 结果以 box-drawing 表格显示
- Readline 风格行编辑（raw mode via crossterm）：
  - ↑/↓ 浏览历史（取出后可修改重执行）
  - ←/→ 移动光标，Home/End 跳行首/尾
  - Ctrl-A/E 同 Home/End，Ctrl-U/K 删至行首/尾
  - Ctrl-L 清屏，Ctrl-C 中止当前输入，Ctrl-D 空行退出 / 非空向前删字符
  - Backspace/Delete 正常工作
  - 历史自动去重（连续相同不重复入栈）
- 点命令（`.` 前缀）：
  - `.help` / `?` — 帮助
  - `.exit` / `.quit` — 退出
  - `.history` — 显示本会话 SQL 历史
  - `.clear` / `.cls` — 清屏
  - `.output <file>` — 持久重定向后续 SQL 结果到文件（追加模式）
  - `.output` — 重置回 stdout
  - `.save <file> [format]` — 将最近一次查询结果一次性写入文件（format 可选：table/json/vertical/csv，默认遵循 --format）

**已排除**（用户明确忽略）：`.tables` / `.schema` / `.files`

## Architecture

三层修改，最大化代码复用：

1. **`output.rs`** — 新增 `format_table_boxed()`，box-drawing 字符（`│`、`─`、`┌`、`┐`、`└`、`┘`、`├`、`┤`、`┼`）。保留现有 ASCII `format_table()` 给 one-shot CLI（向后兼容）。
2. **`cli.rs`** — 从 `run_cli` 中抽出**缓冲式** SQL 执行核心，供 interactive 复用：
   - `pub(crate) struct QueryResult { columns, rows, row_count, affected, kind }`
   - `pub(crate) async fn execute_sql_buffered(client, sql) -> Result<QueryResult, String>`
   - `pub(crate) fn render_result(result, writer: &mut dyn Write, format, boxed: bool) -> Result<(), String>`
   - **不动 one-shot CLI 的 CSV/Vertical 流式路径**（`run_cli` 内部保持原样）。
3. **`interactive.rs`** (新建) — REPL 主体，包含：
   - `LineEdit`：行缓冲区 + 光标位置 + 渲染
   - `History`：历史栈 + 去重 + ↑/↓ 导航索引
   - `SqlTokenizer`：semicolon-aware 切分（识别 `'...'` / `"..."` / `--` / `/* */`）
   - REPL 循环 + 点命令分派
   - `.output` 持久重定向状态 + `.save` 缓存最近一次 `QueryResult`
4. **`main.rs`** — `Cli` 子命令新增 `#[arg(short, long)] interactive: bool`，若为 true 则调用 `interactive::run_interactive(args).await`（复用 `CliArgs`）；否则走原 `run_cli`。
5. **`Cargo.toml`** — 新增 `crossterm = "0.28"` 依赖（仅 `gaussdb-mcp`，不进 workspace lib crates，wasm32 check 不受影响——CI 的 wasm32 检查只针对 `tokio-opengauss`）。

## 关键设计决策

### D1: semicolon-aware 切分
用户可能输入 `SELECT 1; SELECT 2;`，需逐条执行。规则：
- 跟踪单引号字符串 `'foo'`（含 `''` 转义）
- 跟踪双引号标识符 `"my col"`
- 跟踪行注释 `-- ...` 至行尾
- 跟踪块注释 `/* ... */`（可嵌套，PostgreSQL 语义）
- 仅在以上上下文**之外**的 `;` 处切分
- 空语句（连续 `;;`、纯空白）跳过

### D2: `.output` 持久重定向
- 状态：`output_target: OutputTarget { Stdout, File(File) }`
- 切换到文件：打开（追加模式 `OpenOptions::new().create(true).append(true)`），后续 SQL 结果写文件
- 切换回 stdout：`.output`（无参）
- 点命令的反馈信息始终写 stdout（只有 SQL 结果走 redirect）
- SQL 错误始终写 stderr
- 文件打开失败：stderr 报错，保持原 target 不变

### D3: `.save` 一次性保存
- 缓存：`last_query_result: Option<QueryResult>`
- 每次 SQL 执行成功后更新（DML/DDL 也缓存 affected 行数）
- `.save <file> [format]`：序列化缓存到文件（覆盖写），不改变 `.output` 状态
- 无缓存时：stderr 提示 "no previous query result"
- 默认 format = 当前 `--format` 设置

### D4: 渲染统一接口
`render_result(result, writer, format, boxed)` 支持四种 format：
- `Table` + `boxed=true` → 调用 `format_table_boxed`
- `Table` + `boxed=false` → 调用现有 `format_table`（ASCII）
- `Json` → JSON pretty print
- `Vertical` → `-[ RECORD N ]-` 格式（复用现有逻辑，但抽成函数）
- `Csv` → 客户端从缓冲行生成（RFC 4180，与 one-shot CLI 的 server-side COPY 不同；交互模式不适合大数据量导出，README 已有提示）

Interactive REPL 默认 `boxed=true`。`.save` 写文件时也用 `boxed=true`（除非 format 非 table）。

### D5: 终端状态恢复（必须）
raw mode 必须在以下情况下恢复 cooked mode：
- 正常退出（`.exit` / `.quit` / EOF / 空行 Ctrl-D）
- panic（用 RAII guard：`struct RawModeGuard` impl `Drop`，离开作用域时 `disable_raw_mode`）
- Ctrl-C：仅清空当前输入缓冲区回到新提示符，**不退出**

实现：进入 REPL 前 `enable_raw_mode` + `EnterAlternateScreen` 否（不用 alt screen，只是 raw），构造 guard，REPL 结束自动恢复。

### D6: 历史去重
连续相同的语句不重复入栈（非连续的相同语句允许）。比较前 trim。`.history` 显示带序号（1-based）。

### D7: 多字节字符
crossterm 的 `Event::Key(KeyEvent { kind: KeyEventKind::Press, .. })` 配合 `KeyEvent.char()` 已处理 UTF-8 解码。只在 `Press` 事件响应（避免 Release/Repeat 双触发）。删除以 `char` 为单位（`pop` 行尾 / `remove` 光标处），不是字节。

### D8: 一致性 — 不改 one-shot CLI 行为
`gaussdb-mcp cli --sql "..."` 的输出格式、CSV server-side 流式、垂直流式等**完全不变**。本次只新增代码路径，不重构已有路径（除抽出 `execute_sql_buffered` 等共享函数）。

## Tasks 分解

### Task 1 — output.rs 新增 box-drawing 表格 + 单测

**Files:**
- Modify: `tools/gaussdb-mcp/src/output.rs`

**Steps:**
1. 新增 `pub(crate) fn format_table_boxed(columns: &[String], rows: &[Vec<Value>]) -> String`
2. 使用 Unicode box-drawing：`┌─┬─┐`、`│ │ │`、`├─┼─┤`、`└─┴─┘`
3. 列宽计算与现有 `format_table` 相同（`col_widths[i].max(...)`）
4. NULL → `NULL`，与 ASCII 版一致
5. 添加单测：空表、单列单行、多列多行、Unicode 内容（中文）、NULL 值

**Validation:** `cargo test -p gaussdb-mcp output::tests::boxed`

### Task 2 — cli.rs 抽出缓冲执行 + 渲染接口

**Files:**
- Modify: `tools/gaussdb-mcp/src/cli.rs`

**Steps:**
1. 新增 `pub(crate) enum ResultKind { Query, Execute }`
2. 新增 `pub(crate) struct QueryResult { pub columns: Vec<String>, pub rows: Vec<Vec<Value>>, pub row_count: usize, pub affected: u64, pub kind: ResultKind }`
3. 新增 `pub(crate) async fn execute_sql_buffered(client: &tokio_opengauss::Client, sql: &str) -> Result<QueryResult, String>`
   - 复用现有 `format_row_value`、`format_sql_error` 逻辑
   - SELECT/EXPLAIN/WITH → Query；其它 → Execute
   - 全缓冲（不流式）
4. 新增 `pub(crate) fn render_result(result: &QueryResult, writer: &mut dyn std::io::Write, format: OutputFormat, boxed: bool) -> Result<(), String>`
   - Table / boxed=true → `format_table_boxed`
   - Table / boxed=false → 现有 `format_table`
   - Json → pretty JSON
   - Vertical → `-[ RECORD N ]-` 格式
   - Csv → RFC 4180 客户端生成（用 `csv` crate）
5. **不改动 `run_cli`** 的现有逻辑（保持 CSV/Vertical 流式）。`run_cli` 内部 Table/Json 路径**可选**改用新接口（若简化清晰则改，否则保留）。
6. 添加单测：`execute_sql_buffered` 不连 DB 不易测，主要测 `render_result` 各 format。

**Validation:** `cargo test -p gaussdb-mcp cli::tests`；`cargo fmt --check`；`cargo clippy -p gaussdb-mcp --all-targets -- -Dwarnings`

### Task 3 — Cargo.toml 添加 crossterm

**Files:**
- Modify: `tools/gaussdb-mcp/Cargo.toml`

**Steps:**
1. 在 `[dependencies]` 末尾添加 `crossterm = "0.28"`
2. `cargo build -p gaussdb-mcp` 验证编译通过

**Validation:** `cargo build -p gaussdb-mcp`

### Task 4 — interactive.rs 主模块

**Files:**
- Create: `tools/gaussdb-mcp/src/interactive.rs`
- Modify: `tools/gaussdb-mcp/src/main.rs`（声明 `mod interactive;` + 添加 `interactive` 标志 + 分派）

**Subtasks:**

#### 4.1 SqlTokenizer
- `fn split_statements(input: &str) -> Vec<String>`
- 实现 D1 规则；纯空白/空结果过滤
- 单测：`SELECT 1;` → `["SELECT 1"]`；`SELECT ';'; ` → `["SELECT ';'"]`；`SELECT "--x"; ` → `['SELECT "--x"']`；`SELECT 1; SELECT 2;` → 两条；`SELECT 1 -- comment\n;` → `["SELECT 1 -- comment\n"]`

#### 4.2 History
- `struct History { entries: Vec<String>, nav_index: Option<usize> }`
- `push(s: String)`：trim 后若与栈顶相同则不推
- `prev() -> Option<&str>` / `next() -> Option<&str>`：↑/↓ 导航（返回末尾后回到空输入）
- `list(&self) -> impl Iterator`：`.history` 命令显示

#### 4.3 LineEditor
- raw mode + crossterm `event::read()` 循环
- 状态：`buffer: String`、`cursor: usize`（char 索引）
- 渲染：`\r` + 清当前行（`\x1b[2K`）+ 提示符 + buffer + `\x1b[<N>D` 回退到光标位置
- 按键映射：
  - `Char(c)` → insert at cursor
  - `Backspace` → remove char before cursor
  - `Delete` → remove char at cursor
  - `Left` / `Ctrl-B` → cursor -= 1
  - `Right` / `Ctrl-F` → cursor += 1
  - `Home` / `Ctrl-A` → cursor = 0
  - `End` / `Ctrl-E` → cursor = buffer.len()
  - `Ctrl-U` → delete to start (clear)
  - `Ctrl-K` → delete from cursor to end
  - `Ctrl-L` → clear screen (`\x1b[2J\x1b[H`) + 重绘
  - `Up` / `Ctrl-P` → history prev (replace buffer)
  - `Down` / `Ctrl-N` → history next
  - `Enter` → 返回 `Some(buffer.clone())`（不含换行）
  - `Ctrl-C` → 返回 `None`（中断信号，调用方清空 accumulated SQL）
  - `Ctrl-D` → buffer 空 → 返回 `Err(ReadEof)`；非空 → 同 Delete
- 仅响应 `KeyEventKind::Press`
- 多行语义：LineEdit 处理**单行**输入，多行累积在 REPL 层（用提示符 `$ ` vs `> ` 区分）

#### 4.4 RawModeGuard
- `struct RawModeGuard;`
- `impl Drop for RawModeGuard { fn drop: disable_raw_mode().ok(); }`
- 进入 REPL 前构造，保证 panic / early return 恢复终端

#### 4.5 REPL loop
```rust
pub(crate) async fn run_interactive(args: CliArgs) -> Result<(), String> {
    // 1. 同 run_cli 的步骤 2-6：加载 config → resolve target → connect → keychain migration
    // 2. 进入 raw mode + 构造 guard
    // 3. 打印欢迎信息（连接名、版本、`.help` 提示）
    // 4. 循环：
    //    - 用 LineEditor 读一行（提示符根据是否在多行中选 `$ ` 或 `> `）
    //    - 累加到 buffer，末尾加 '\n'
    //    - 调 split_statements(&buffer)：
    //        * 若最后一条无闭合 `;`（即尾部还有未完成语句）→ 继续读（提示符 `> `）
    //        * 否则对每条完整语句执行：
    //            - 点命令 → 分派
    //            - SQL → execute_sql_buffered → render_result → 缓存到 last_query_result
    //          清空 buffer，提示符回 `$ `
    // 5. .exit / .quit / EOF / Ctrl-D on empty → break
}
```

#### 4.6 点命令分派
```rust
fn handle_dot_command(
    line: &str,
    ctx: &mut ReplContext,  // 含 last_query_result, output_target, history, client, format
) -> CommandAction {  // Continue / Exit
}
```
- `.help` / `?` → 打印所有命令说明到 stdout
- `.exit` / `.quit` → `CommandAction::Exit`
- `.history` → 打印 `history.list()`（带序号）
- `.clear` / `.cls` → `\x1b[2J\x1b[H`
- `.output [<file>]`：
  - 有参 → 切换 output_target 到文件（追加模式）；失败 → stderr 报错保持原状
  - 无参 → 切回 stdout
- `.save <file> [format]`：
  - 取 last_query_result；None → stderr 报错
  - 解析 format（可选，默认 `--format`）
  - 覆盖写文件（`File::create`）
  - 不改变 output_target
- 未识别 `.xxx` → stderr 提示 "unknown command, .help for list"

#### 4.7 main.rs 集成
```rust
Commands::Cli {
    sql, file, check_connection, verbose, format,
    statement_timeout, connection_max_lifetime, timeout_action,
    interactive,  // 新增字段
} => {
    if check_connection { /* 原逻辑 */ }
    else if interactive {
        let fmt = format.parse().unwrap_or(OutputFormat::Table);
        let args = CliArgs { sql, file, connection_name: cli.name, config_path: cli.config, format: fmt, statement_timeout, connection_max_lifetime, timeout_action };
        if let Err(e) = interactive::run_interactive(args).await {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    } else { /* 原 run_cli 逻辑 */ }
}
```

**Validation:**
- `cargo build -p gaussdb-mcp`
- `cargo fmt --all -- --check`
- `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets`
- `cargo test -p gaussdb-mcp`（含 SqlTokenizer 单测）
- 手测（需 DB）：`docker compose up -d` → `cargo run -p gaussdb-mcp -- cli -i`，测试多行 SQL、点命令、↑/↓ 历史、Ctrl 组合键、`.output file.log` + 多条 SQL、`.save out.csv csv`

### Task 5 — README + AGENTS 更新

**Files:**
- Modify: `tools/gaussdb-mcp/README.md`（在 CLI Mode 节加 `--interactive / -i` 说明）
- 不改 AGENTS.md（CI 命令不变）

**内容：** 新增 `--interactive` 选项说明 + 示例 + 点命令列表 + 提示符语义。

## Execution Order

1. **Task 1 + Task 2 + Task 3** 可并行（互相独立）：
   - T1 改 output.rs（新增函数）
   - T2 改 cli.rs（新增函数，不改 run_cli 行为）
   - T3 改 Cargo.toml
2. **Task 4** 依赖 1+2+3 完成（一个 agent 完成整个 interactive.rs + main.rs 集成，保持上下文一致）
3. **Task 5** 最后（文档）

## Anti-Goals / Out of Scope

- ❌ 不加 `.tables` / `.schema` / `.files`（用户明确排除）
- ❌ 不持久化历史到磁盘文件（仅会话内，YAGNI）
- ❌ 不实现 psql-style `\x` 切换垂直输出（用户可用 `.save out vertical` 或改 `--format vertical` 后重启 REPL）
- ❌ 不改 one-shot CLI (`run_cli`) 的输出格式或 CSV 流式行为
- ❌ 不支持流式 COPY 到 `.output` 文件（交互模式全缓冲，大数据量用 one-shot CLI）

## Risks

- **R1: 终端兼容性**：crossterm 在某些终端（Windows cmd.exe 老 version、CI 无 TTY）可能行为不一致。Mitigation：手测在 macOS Terminal/iTerm，CI 不跑交互测试（仅单测）。
- **R2: UTF-8 边界**：光标按 char 推进，渲染按 char 计算 width（东亚宽字符宽度问题暂不处理——大多数 SQL 是 ASCII；若用户输入中文，渲染可能轻微错位，可接受 MVP）。
- **R3: panic 时终端状态**：RawModeGuard + `Drop` 保证恢复；同时 main 中用 `catch_unwind` 不必要（panic 仍会走 Drop）。
- **R4: crossterm 与 wasm32**：wasm32 CI 只检 `tokio-opengauss`，不检 `gaussdb-mcp`。crossterm 不会影响 wasm32 check。

## Acceptance Criteria

- [ ] `gaussdb-mcp cli -i` 进入 REPL，提示符 `$ `
- [ ] 多行 SQL（`SELECT 1,\n2,\n3;`）能正确累积并执行
- [ ] `SELECT 1; SELECT 2;` 一行两条语句都执行
- [ ] `SELECT ';' AS a;` 不被误切分
- [ ] ↑/↓ 能浏览历史，Ctrl-A/E/U/K/L 各司其职
- [ ] Ctrl-C 中止当前输入但不退出 REPL，Ctrl-D 空行退出
- [ ] `.help` / `?` / `.exit` / `.quit` / `.history` / `.clear` 工作正常
- [ ] `.output out.log` 后续 SQL 写文件；`.output` 重置回 stdout
- [ ] `.save out.csv csv` 把最近查询写为 CSV
- [ ] SQL 错误显示后 REPL 继续
- [ ] `cargo fmt --check` + `RUSTFLAGS="-Dwarnings" cargo clippy --all --all-targets` 通过
- [ ] `cargo test -p gaussdb-mcp` 通过
- [ ] `cargo check --target wasm32-unknown-unknown --manifest-path crates/tokio-opengauss/Cargo.toml --no-default-features --features js` 不受影响

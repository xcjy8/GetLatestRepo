# Changelog

---

## [0.1.5] - 2026-04-22

> 基于数据验证结果，彻底移除 git2 网络 fetch 路径

### Changed

- `git.rs`: `fetch_detailed` 直接走 `fetch_with_git_command`，不再尝试 git2
- `fetcher.rs`: 移除 git2 fallback 缓存、fallback 信息汇总等配套逻辑
- `git.rs`: 保留 `fetch_with_git2` 作为预留接口（标记 `#[allow(dead_code)]`），本地操作未来仍可能使用

### Fixed

- 修复双层架构导致的每个仓库浪费 3 秒等待问题（git2 平均 1600ms > 原生 git 1200ms，且无优势）
- 修复 5 并发下 git2 部分仓库耗时翻倍的问题（如 JustAnime 888ms→3491ms）

---

## [0.1.4] - 2026-04-22

> fetch 双层架构、进度条精简与 git2 偏好缓存

### Added

**fetch 双层架构（git2 快速路径 + 原生 git 命令兜底）**

- `git.rs`: `fetch_detailed` 改为三层策略
  1. 若仓库已在 `git2_fallback_cache` 中，直接走原生 `git fetch`
  2. 否则启动 git2 fetch，3 秒超时监控
  3. git2 失败/超时时 fallback 到 `fetch_with_git_command`
- `git.rs`: 新增 `fetch_with_git_command`，使用 `std::process::Command` 执行 `git fetch origin`
  - 支持 `child.kill()` 强制终止，避免 git2 无法中断的问题
  - 代理通过环境变量 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` 传递，兼容旧版本 git
  - 设置 `GIT_TERMINAL_PROMPT=0` 防止交互式阻塞
- `git.rs`: `FetchStatus` 统一错误分类（`classify_error`），兼容 git2 和原生 git 的错误文本

**git2 偏好缓存**

- `fetcher.rs`: `Fetcher` 新增 `git2_fallback_cache: Arc<Mutex<HashSet<String>>>`
- 只要某仓库发生过 git2 → git 命令的 fallback，路径即写入缓存
- 下次 fetch 同一仓库时直接跳过 git2，避免重复浪费 3 秒超时等待
- 缓存为进程级（随 `Fetcher` 实例生命周期），不持久化到数据库

### Changed

**进度条与输出排版**

- `fetcher.rs`: 去掉 `MultiProgress`，改用单个 `ProgressBar`
- `fetcher.rs`: fetch 过程中不再穿插任何 `pb.println` 输出，进度条保持干净
- `fetcher.rs`: 所有 fallback / 失败 / 移动 / 恢复信息在进度条 `finish_and_clear()` 后统一树形输出
- `fetcher.rs`: fallback 汇总指明具体仓库名和原始原因（如 `"git2 fetch 3s 内未返回"`）

**进程优雅关闭**

- `main.rs`: 末尾从 `Ok(ExitCode::from(exit_code))` 改为直接 `std::process::exit(exit_code)`
- `signal_handler.rs`: 补充注释，说明后台 `tokio::spawn` 的 `ctrl_c()` 监听任务会导致 tokio runtime 在 main 返回后无法退出

**代理兼容性**

- `git.rs`: 原生 git 命令路径不再使用 `git -c http.proxy=`（旧版本 git 不支持），改用环境变量传递代理

### Fixed

- `fetcher.rs`: 修复 `fallback_reason` 在重试时可能丢失的问题，始终保留第一次的原始 fallback 原因
- `fetcher.rs`: 修复缓存策略过保守的问题，fallback 发生后即写入缓存（不限于成功时）
- `workflow/executor.rs`: 修复 pull-force 冲突恢复命令的树形输出格式

---

## [0.1.3] - 2026-04-21

> package.sh 新增打包脚本

### Added


**优雅关闭（Graceful Shutdown）三层策略**

解决 1000 仓库场景下 Ctrl+C 无法停止的核心痛点：

1. **密集检查点** — `fetcher.rs` 结果收集循环改用 `timeout(200ms)` 轮询，检测到 shutdown 立即 break；`fetch_and_rescan` repo 循环、`concurrent.rs` 线程创建循环、`workflow/executor.rs` Pull 安全检查循环均加入 `is_shutdown_requested()` 检查
2. **main 末尾自动 exit** — 命令执行返回后若 shutdown 标志已设置，直接 `process::exit(0)`，不等 tokio runtime 等待后台 `spawn_blocking` 线程
3. **10 秒兜底 + 双按 Ctrl+C** — `signal_handler.rs` 第一次 Ctrl+C 设标志并启动 10 秒定时器；第二次 Ctrl+C 或 10 秒超时均立即 `process::exit(130)`

**启动自检（Startup Cleanup）**

- `main.rs` 新增 `run_startup_cleanup()`：打开数据库后遍历所有记录
  - 若记录路径不存在但 `needauth/` 下有同名仓库 → 自动修复路径
  - 若路径不存在且 needauth 下也没有 → 删除孤儿记录
  - 遍历所有 `scan_sources` 的 `needauth/` 目录，清理 `.getlatestrepo_swap` 残留临时目录
- 自检仅在非 `init` 命令时执行，避免初始化前误操作

### Changed

- `signal_handler.rs`: 重写为三层关闭策略，原 `AtomicBool` 单标志升级为 `tokio::select!` 竞争模型
- `fetcher.rs`: `fetch_all_detailed` 结果收集从裸 `futures.next().await` 改为 `timeout` 轮询
- `concurrent.rs`: 线程创建循环增加 shutdown 检查，剩余任务直接发 `None`

---

## [0.1.2] - 2026-04-21

### Fixed

> 14 项缺陷修复（安全/并发/Git 状态/信号/阻塞 IO）

**安全 (Critical):**

- `fetcher`: `move_repo_to_needauth` / `move_repo_from_needauth` 新增 `expected_parent` 参数，拒绝绝对路径遍历攻击
- `fetcher`: 回滚恢复失败不再静默忽略，返回 CRITICAL 错误并告知用户临时路径位置

**并发/异步 (High):**

- `fetcher`: 所有 `spawn_blocking`（scan/move/inspect/fetch）包裹 `timeout`，防止 Semaphore 泄漏导致软死锁
- `fetcher`: 重试总时间限制为 `timeout_secs * 2`，避免指数退火导致超时失控
- `fetcher`: `fetch_and_update` DB 循环移至 `spawn_blocking`，避免阻塞异步运行时
- `reporter`: `save_report` 新增 `save_report_async`，文件写入不再阻塞 async 线程
- `status`: `--issues` 的 `db.list_repositories()` 包裹 `spawn_blocking`
- `concurrent`: 线程栈大小降至 1MB，减少被遗弃线程的内存泄漏

**Git 状态 (High):**

- `git`: `pull_ff_only` / `pull_force` 在 `set_target` 失败后自动回滚 `checkout_tree` 到原始提交，防止工作目录与 HEAD 不一致
- `git`: `pull_force` pull 失败后若 stash 已创建，主动警告用户 stash 名称和恢复命令，避免孤儿 stash
- `git`: `find_stash_index` / `get_conflict_files` 错误分支改为显式警告，不再静默吞掉错误

**其他修复 (Medium):**

- `models`/`scanner`/`config`/`db`/`sync`: `max_depth` 类型从 `i32` 改为 `usize`，消除负值 round-trip 导致深度限制失效的 bug
- `signal_handler`/`workflow`/`fetcher`: 移除 `#[allow(dead_code)]`，`SHUTDOWN_REQUESTED` 在 workflow 步骤循环和 fetch future 生成循环中实际生效
- `workflow/executor`: 两处 `let _ =`（`ensure_reports_dir`、`upsert_repository`）改为显式错误日志

### Notes

- `fetcher`: 恢复路径存在设计限制——假设原始仓库是扫描根的直接子目录。若原始路径为嵌套目录，恢复后位置可能不正确。完整修复需 DB schema 变更以保存原始相对路径。
- `reporter`/`scanner`: `Path::exists()` 为阻塞文件系统 I/O，已在代码中添加注释说明。

---

## [0.1.1] - 2026-04-21

### Refactor

> P0/P1/P2 全量修复与安全重构

P0 修复:

- scanner/sync: 修复 *.txt glob 匹配导致目录被全部过滤的 bug
- commands/scan: --depth 参数正确传递至 Scanner
- main/workflow/signal: 移除 process::exit，确保 flock 文件锁正常释放
- fetcher/scanner/executor: 所有 git2/fs 阻塞操作移至 spawn_blocking
- concurrent: 实现真实任务级超时（超时后放弃线程，避免永久阻塞）
- fetcher/scanner: needauth 移动后保留 DB 记录，cleanup 不再误删

P1 改进:

- db: 文件权限 0o600、WAL + synchronous=NORMAL + temp_store=MEMORY
- db/models: dirty_files 从换行分隔迁移至 JSON 数组（向后兼容解析）
- security: once_cell::Lazy 缓存敏感模式集，扩展 .env/.pem/CI 等检测
- 全仓库: 修正错别字、统一输出图标、中文格式化时长
- git/reporter: upstream_url 和路径脱敏，防止敏感信息泄漏
- utils: 提取 NEEDAUTH_DIR/DEFAULT_PROXY_URL 等共享常量
- fetcher/executor: TTY 检测避免非交互环境 stdin 挂起

P2 清理:

- workflow/executor: 提取 RepoChangeView trait + print_repo_change_tree，
消除 execute() 与 execute_pull_safe() 中 ~150 行脏仓库树形渲染重复

---

## [0.1.0] - 2026-04-09

### Added

- 🎉 Initial release of GetLatestRepo.

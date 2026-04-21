# Changelog

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

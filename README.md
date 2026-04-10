<div align="center">

# GetLatestRepo

[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange?logo=rust)](https://www.rust-lang.org)
[![CI](https://github.com/xcjy8/GetLatestRepo/actions/workflows/ci.yml/badge.svg)](https://github.com/xcjy8/GetLatestRepo/actions)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

**A fast and elegant local Git repository manager written in Rust.**

[English](README.md) · [简体中文](README.zh-CN.md)

</div>

---

## ✨ Features

- 🔍 **Recursive Scanning** — Discover all Git repositories under any directory in seconds.
- ⚡ **Concurrent Fetch** — Parallel fetches with configurable concurrency and per-request proxy support.
- 🛡️ **Safety First** — `pull-safe` skips dirty repos; `pull-force` auto-stashes changes. Built-in security scanning detects deletion risks, sensitive file changes, suspicious code patterns, and unknown committers.
- 📊 **Beautiful Reports** — Export scan results as terminal tables, HTML, or Markdown. Auto-archived by date.
- 🔄 **Workflow Engine** — Built-in workflows (`daily`, `check`, `report`, `ci`, `pull-safe`, `pull-force`) to automate your routine.
- 🗃️ **SQLite Cache** — Fast local caching with WAL mode. No re-scanning unless necessary.
- 🔒 **Process Lock** — Prevents multiple instances from running simultaneously.
- 🌐 **Proxy Support** — Per-request proxy configuration without polluting global environment variables.

---

## 📸 Screenshots

<p align="center">
  <img src="docs/images/01.jpg" alt="Terminal Table Report" width="80%">
</p>

<p align="center">
  <img src="docs/images/02.jpg" alt="HTML Dark Theme Report" width="80%">
</p>

<p align="center">
  <img src="docs/images/03.jpg" alt="Workflow Execution" width="80%">
</p>

---

## 🚀 Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/xcjy8/GetLatestRepo.git
cd GetLatestRepo

# Build release binary
cargo build --release

# Install to /usr/local/bin (optional)
sudo cp target/release/getlatestrepo /usr/local/bin/
```

### Prerequisites

- Rust 1.70 or newer
- `git` installed on your system

---

## 🏁 Quick Start

```bash
# 1. Initialize a scan source
getlatestrepo init ~/projects

# 2. Run the daily workflow (scan + fetch + status check)
getlatestrepo workflow daily

# 3. Generate an HTML report
getlatestrepo workflow report
```

---

## 📖 Command Overview

### Global Flags

These flags are available on every command:

| Flag | Description |
|------|-------------|
| `--proxy` | Enable the default proxy (`http://127.0.0.1:7890`). |
| `--proxy-url <URL>` | Specify a custom proxy address (e.g. `http://127.0.0.1:1080`). |
| `--no-security-check` | Disable the pre-fetch/pre-pull security scan. |

### Commands

| Command | Description |
|---------|-------------|
| `getlatestrepo init <path>` | Add a directory to scan sources. |
| `getlatestrepo scan` | Recursively find Git repos and persist to the local database. |
| `getlatestrepo fetch` | Concurrently fetch all tracked repositories. |
| `getlatestrepo status <path>` | Inspect a single repository in detail. |
| `getlatestrepo config` | Manage scan sources, ignore patterns, and settings. |
| `getlatestrepo workflow <name>` | Run a built-in or custom workflow. |
| `getlatestrepo discard` | Interactively discard local changes. |

### Command Options

#### `init`

```bash
getlatestrepo init <PATH>
```

| Argument | Description |
|----------|-------------|
| `<PATH>` | The root directory to scan for Git repositories. |

#### `scan`

```bash
getlatestrepo scan [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--fetch` | Run fetch before scanning. |
| `-o, --output <FORMAT>` | Output format: `terminal` (default), `html`, or `markdown`. |
| `--out <PATH>` | Custom output file path (default auto-generated). |
| `-d, --depth <N>` | Limit scan depth. |
| `-j, --jobs <N>` | Concurrency limit (default: 5). |

#### `fetch`

```bash
getlatestrepo fetch [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `-j, --jobs <N>` | Concurrency limit (default: 5). |
| `-t, --timeout <SECS>` | Timeout per fetch in seconds (default: 30). |

#### `status`

```bash
getlatestrepo status <PATH> [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--diff` | Show diff content. |

#### `config`

```bash
getlatestrepo config <SUBCOMMAND>
```

| Subcommand | Description |
|------------|-------------|
| `add <PATH>` | Add a new scan source. |
| `list` | List all configured scan sources. |
| `remove <PATH_OR_ID>` | Remove a scan source by path or ID. |
| `ignore <PATTERNS>` | Set global ignore rules (comma-separated). |
| `path` | Show the configuration file location. |

#### `workflow`

```bash
getlatestrepo workflow [NAME] [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--list` | List all available workflows. |
| `--dry-run` | Show the execution plan without running it. |
| `--silent` | Silent mode (returns only exit code). |
| `-j, --jobs <N>` | Override default concurrency. |
| `-t, --timeout <SECS>` | Override default timeout. |
| `--yes` | Auto-confirm prompts (only for `pull-safe`). |
| `--diff-after` | Show new commits after pull (only for `pull-safe` / `pull-force`). |
| `--no-pull-guard` | Disable pull safety check (only for `pull-safe`). |

#### `discard`

```bash
getlatestrepo discard [PATH] [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--yes` | Skip the confirmation prompt. |

### Built-in Workflows

| Workflow | What it does |
|----------|--------------|
| `daily` | Fetch → Scan → Show a concise status summary. |
| `check` | Scan only (no fetch), show repositories needing attention. |
| `report` | Fetch → Scan → Generate an HTML/Markdown report. |
| `ci` | Fetch → Scan → Check behind (returns error code if behind > 0). |
| `pull-safe` | Fetch → Safe pull (ff-only, skips dirty repositories). |
| `pull-force` | Fetch → Force pull (stash → pull → pop). |

---

## 📁 Reports

Generated reports are automatically archived under:

```
reports/YYYY/MM/DD/getlatestrepo-report-YYYYMMDD-HHMMSS.<ext>
```

A `reports/latest.html` symlink always points to the newest HTML report.

---

---

## 🤝 Contributing

Contributions are welcome! Please feel free to open issues or submit pull requests.

---

## License

This project is dual-licensed:

- **AGPL-3.0-or-later** — For open-source and non-commercial use. See [LICENSE](LICENSE) for the full text.
- **Commercial License** — For proprietary/closed-source or commercial use, please contact the author for a commercial license.

If you wish to use this software in a commercial product or service without disclosing your source code, you must obtain a separate commercial license from the copyright holder.

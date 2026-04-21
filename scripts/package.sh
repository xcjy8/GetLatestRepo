#!/bin/bash
# GetLatestRepo 打包部署脚本
# 将编译后的 Release 二进制文件移动至指定目录

set -euo pipefail

# ═══════════════════════════════════════════════════
# 颜色定义
# ═══════════════════════════════════════════════════
CLR_RESET=$'\033[0m'
CLR_GREEN=$'\033[0;32m'
CLR_RED=$'\033[0;31m'
CLR_YELLOW=$'\033[0;33m'
CLR_BLUE=$'\033[0;34m'
CLR_CYAN=$'\033[0;36m'
CLR_DIM=$'\033[2m'

# ═══════════════════════════════════════════════════
# 配置
# ═══════════════════════════════════════════════════
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE_BIN="${PROJECT_ROOT}/target/release/getlatestrepo"
TARGET_DIR="/Users/sy/aibin"
TARGET_BIN="${TARGET_DIR}/getlatestrepo"

# ═══════════════════════════════════════════════════
# 辅助函数
# ═══════════════════════════════════════════════════
step_start() {
    local num="$1"
    local total="$2"
    local desc="$3"
    echo ""
    echo "${CLR_CYAN}├─ [步骤 ${num}/${total}] ${desc}${CLR_RESET}"
}

step_detail() {
    local icon="$1"
    local label="$2"
    local value="$3"
    echo "${CLR_BLUE}│  ${icon} ${label}${CLR_RESET}${CLR_DIM}: ${value}${CLR_RESET}"
}

step_ok() {
    local msg="${1:-成功}"
    echo "${CLR_BLUE}│  ${CLR_GREEN}✓${CLR_RESET} ${CLR_DIM}状态${CLR_RESET}${CLR_DIM}: ${CLR_GREEN}${msg}${CLR_RESET}"
}

step_warn() {
    local msg="$1"
    echo "${CLR_BLUE}│  ${CLR_YELLOW}⚠${CLR_RESET} ${CLR_DIM}状态${CLR_RESET}${CLR_DIM}: ${CLR_YELLOW}${msg}${CLR_RESET}"
}

step_err() {
    local msg="$1"
    echo "${CLR_BLUE}│  ${CLR_RED}✗${CLR_RESET} ${CLR_DIM}状态${CLR_RESET}${CLR_DIM}: ${CLR_RED}${msg}${CLR_RESET}"
}

print_banner() {
    echo ""
    echo "${CLR_CYAN}╔══════════════════════════════════════════════════╗${CLR_RESET}"
    echo "${CLR_CYAN}║${CLR_RESET}     ${CLR_YELLOW}GetLatestRepo 打包部署脚本${CLR_RESET}              ${CLR_CYAN}║${CLR_RESET}"
    echo "${CLR_CYAN}╚══════════════════════════════════════════════════╝${CLR_RESET}"
    echo ""
    echo "${CLR_YELLOW}📦 打包流程开始${CLR_RESET}"
}

print_summary() {
    local result="$1"
    local elapsed="$2"
    local size="$3"
    local version_info="$4"

    echo ""
    echo "${CLR_CYAN}└─${CLR_RESET} ${CLR_YELLOW}📊 打包总结${CLR_RESET}"
    echo "   ${CLR_BLUE}├─${CLR_RESET} ${CLR_DIM}版本信息${CLR_RESET}${CLR_DIM}: ${version_info}${CLR_RESET}"
    echo "   ${CLR_BLUE}├─${CLR_RESET} ${CLR_DIM}文件大小${CLR_RESET}${CLR_DIM}: ${size}${CLR_RESET}"
    echo "   ${CLR_BLUE}├─${CLR_RESET} ${CLR_DIM}总耗时${CLR_RESET}${CLR_DIM}: ${elapsed}${CLR_RESET}"
    if [ "$result" = "ok" ]; then
        echo "   ${CLR_BLUE}└─${CLR_RESET} ${CLR_DIM}结果${CLR_RESET}${CLR_DIM}: ${CLR_GREEN}✓ 全部成功${CLR_RESET}"
    else
        echo "   ${CLR_BLUE}└─${CLR_RESET} ${CLR_DIM}结果${CLR_RESET}${CLR_DIM}: ${CLR_RED}✗ 部分失败${CLR_RESET}"
    fi
    echo ""
}

format_duration() {
    local secs="$1"
    if (( secs < 60 )); then
        echo "${secs}s"
    else
        local m=$(( secs / 60 ))
        local s=$(( secs % 60 ))
        echo "${m}m ${s}s"
    fi
}

# ═══════════════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════════════
main() {
    local START_TIME=$(date +%s)
    local RESULT="ok"
    local VERSION_INFO="unknown"
    local FILE_SIZE="unknown"
    local BACKUP_FILE=""

    print_banner

    # ───────────────────────────────────────────────
    # 步骤 1: 编译 Release 版本
    # ───────────────────────────────────────────────
    step_start "1" "3" "编译 Release 版本"
    step_detail "📁" "工作目录" "${PROJECT_ROOT}"
    step_detail "🔧" "编译命令" "cargo build --release"

    cd "${PROJECT_ROOT}"

    if ! cargo build --release 2>&1 | while IFS= read -r line; do
        echo "${CLR_DIM}│  │  ${line}${CLR_RESET}"
    done; then
        step_err "编译失败"
        RESULT="fail"
        local END_TIME=$(date +%s)
        local ELAPSED=$(( END_TIME - START_TIME ))
        print_summary "${RESULT}" "$(format_duration ${ELAPSED})" "${FILE_SIZE}" "${VERSION_INFO}"
        echo "${CLR_RED}✗ 打包失败：编译阶段出错${CLR_RESET}"
        exit 1
    fi

    # 验证二进制文件
    if [ ! -f "${SOURCE_BIN}" ]; then
        step_err "找不到编译产物: ${SOURCE_BIN}"
        RESULT="fail"
        local END_TIME=$(date +%s)
        local ELAPSED=$(( END_TIME - START_TIME ))
        print_summary "${RESULT}" "$(format_duration ${ELAPSED})" "${FILE_SIZE}" "${VERSION_INFO}"
        echo "${CLR_RED}✗ 打包失败：编译产物缺失${CLR_RESET}"
        exit 1
    fi

    FILE_SIZE=$(ls -lh "${SOURCE_BIN}" | awk '{print $5}')
    step_ok "编译成功"

    # ───────────────────────────────────────────────
    # 步骤 2: 准备目标目录
    # ───────────────────────────────────────────────
    step_start "2" "3" "准备目标目录"
    step_detail "📁" "目标目录" "${TARGET_DIR}"

    if [ ! -d "${TARGET_DIR}" ]; then
        step_detail "🔧" "操作" "创建目录 ${TARGET_DIR}"
        if mkdir -p "${TARGET_DIR}" 2>/dev/null; then
            step_ok "目录创建成功"
        else
            step_err "无法创建目录 ${TARGET_DIR}"
            RESULT="fail"
            local END_TIME=$(date +%s)
            local ELAPSED=$(( END_TIME - START_TIME ))
            print_summary "${RESULT}" "$(format_duration ${ELAPSED})" "${FILE_SIZE}" "${VERSION_INFO}"
            echo "${CLR_RED}✗ 打包失败：无法创建目标目录${CLR_RESET}"
            exit 1
        fi
    else
        step_detail "📋" "目录状态" "已存在"
    fi

    # 备份原有文件
    if [ -f "${TARGET_BIN}" ]; then
        local TIMESTAMP=$(date +%Y%m%d-%H%M%S)
        BACKUP_FILE="${TARGET_BIN}.bak.${TIMESTAMP}"
        if cp "${TARGET_BIN}" "${BACKUP_FILE}" 2>/dev/null; then
            step_detail "💾" "备份操作" "getlatestrepo → getlatestrepo.bak.${TIMESTAMP}"
            step_ok "备份成功"
        else
            step_warn "备份失败（可能无权限），继续执行..."
        fi
    else
        step_detail "📋" "原有文件" "不存在，无需备份"
    fi

    # ───────────────────────────────────────────────
    # 步骤 3: 替换二进制文件
    # ───────────────────────────────────────────────
    step_start "3" "3" "替换二进制文件"
    step_detail "📄" "源文件" "target/release/getlatestrepo (${FILE_SIZE})"
    step_detail "🎯" "目标文件" "${TARGET_BIN}"

    # 获取版本信息（从 Cargo.toml 读取，避免进程锁冲突）
    VERSION_INFO="v$(grep '^version' "${PROJECT_ROOT}/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')" || VERSION_INFO="unknown"

    if mv "${SOURCE_BIN}" "${TARGET_BIN}" 2>/dev/null; then
        chmod +x "${TARGET_BIN}"
        step_ok "替换成功"
    else
        step_err "移动文件失败"
        # 尝试恢复备份
        if [ -n "${BACKUP_FILE}" ] && [ -f "${BACKUP_FILE}" ]; then
            step_detail "🔄" "恢复操作" "从备份恢复..."
            cp "${BACKUP_FILE}" "${TARGET_BIN}" 2>/dev/null || true
        fi
        RESULT="fail"
        local END_TIME=$(date +%s)
        local ELAPSED=$(( END_TIME - START_TIME ))
        print_summary "${RESULT}" "$(format_duration ${ELAPSED})" "${FILE_SIZE}" "${VERSION_INFO}"
        echo "${CLR_RED}✗ 打包失败：文件替换出错${CLR_RESET}"
        exit 1
    fi

    # ───────────────────────────────────────────────
    # 验证
    # ───────────────────────────────────────────────
    step_detail "🔍" "验证" "执行 ${TARGET_BIN} --version"
    local VERIFY_OUTPUT
    VERIFY_OUTPUT=$("${TARGET_BIN}" --version 2>&1) || true
    if echo "${VERIFY_OUTPUT}" | grep -q 'getlatestrepo'; then
        step_detail "📋" "版本输出" "${VERIFY_OUTPUT}"
        step_ok "验证通过"
    elif echo "${VERIFY_OUTPUT}" | grep -q 'already running'; then
        step_detail "📋" "版本输出" "${VERIFY_OUTPUT}"
        step_warn "进程锁冲突，跳过版本验证"
    else
        step_warn "版本验证未返回预期输出"
    fi

    # ───────────────────────────────────────────────
    # 清理旧备份（保留最近 5 个）
    # ───────────────────────────────────────────────
    local BACKUP_COUNT
    BACKUP_COUNT=$(find "${TARGET_DIR}" -maxdepth 1 -name 'getlatestrepo.bak.*' -type f 2>/dev/null | wc -l | tr -d ' ')
    if [ "${BACKUP_COUNT}" -gt 5 ]; then
        find "${TARGET_DIR}" -maxdepth 1 -name 'getlatestrepo.bak.*' -type f -printf '%T@ %p\n' 2>/dev/null | \
            sort -n | head -n -5 | cut -d' ' -f2- | \
            while IFS= read -r oldbak; do
                rm -f "${oldbak}" 2>/dev/null || true
            done
        step_detail "🧹" "清理旧备份" "保留最近 5 个备份，删除 ${BACKUP_COUNT} 个旧备份"
    fi

    # ───────────────────────────────────────────────
    # 总结
    # ───────────────────────────────────────────────
    local END_TIME=$(date +%s)
    local ELAPSED=$(( END_TIME - START_TIME ))

    print_summary "${RESULT}" "$(format_duration ${ELAPSED})" "${FILE_SIZE}" "${VERSION_INFO}"

    if [ "${RESULT}" = "ok" ]; then
        echo "${CLR_GREEN}✓ 打包完成！${CLR_RESET}"
        echo ""
        echo "${CLR_DIM}部署路径${CLR_RESET}: ${CLR_CYAN}${TARGET_BIN}${CLR_RESET}"
        echo "${CLR_DIM}版本信息${CLR_RESET}: ${CLR_CYAN}${VERSION_INFO}${CLR_RESET}"
        echo "${CLR_DIM}备份文件${CLR_RESET}: ${CLR_CYAN}${BACKUP_FILE:-无}${CLR_RESET}"
        exit 0
    else
        echo "${CLR_RED}✗ 打包失败！${CLR_RESET}"
        exit 1
    fi
}

# ═══════════════════════════════════════════════════
# 入口
# ═══════════════════════════════════════════════════
main "$@"

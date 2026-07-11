#!/usr/bin/env bash
# =============================================================================
# 脚本名称: github_mirror_auto.sh
# 功能描述: 自动检测 GitHub 连接状态，连接失败时自动查找可用镜像站并配置环境变量
# 适用系统: Linux/macOS (Bash 4.0+)
# 作者:     Auto-generated
# 日期:     2026-07-11
# =============================================================================

set -euo pipefail

# ============================ 配置区域 ============================
# 脚本配置
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_DIR="${SCRIPT_DIR}/logs"
LOG_FILE="${LOG_DIR}/github_mirror_$(date +%Y%m%d).log"
PID_FILE="${SCRIPT_DIR}/.github_mirror.pid"
TIMEOUT_SECONDS=10          # 连接超时时间
MAX_RETRIES=2               # 重试次数
CHECK_INTERVAL=300          # 守护模式检查间隔(秒)

# GitHub 官方地址
GITHUB_OFFICIAL="https://github.com"
GITHUB_API="https://api.github.com"
GITHUB_RAW="https://raw.githubusercontent.com"

# 镜像站候选列表 (按优先级排序)
# 格式: "域名|类型|描述"
declare -a MIRROR_CANDIDATES=(
    "bgithub.xyz|full|界面简洁，响应速度快，支持仓库搜索"
    "kkgithub.com|full|操作逻辑与官网一致，支持代码高亮"
    "gitclone.com|full|附带Git Clone加速命令，适合开发者"
    "github.ur1.fun|full|加载速度快，支持Markdown文档渲染"
    "kgithub.com|full|支持代码查看、Issue和评论"
    "4github.com|full|Wiki维护的镜像站"
    "gh-proxy.com|proxy|文件下载加速，支持批量"
    "ghproxy.net|proxy|自动识别文件类型，支持断点续传"
    "ghproxy.homeboyc.cn|proxy|适合下载大体积Release包"
    "moeyy.cn/gh-proxy|proxy|CDN加速，下载速度快"
)

# 环境变量配置文件
ENV_FILES=(
    "${HOME}/.bashrc"
    "${HOME}/.bash_profile"
    "${HOME}/.zshrc"
    "${HOME}/.profile"
)

# 颜色定义
readonly RED='\033[0;31m'
readonly GREEN='\033[0;32m'
readonly YELLOW='\033[1;33m'
readonly BLUE='\033[0;34m'
readonly CYAN='\033[0;36m'
readonly NC='\033[0m' # No Color

# ============================ 日志函数 ============================

log_init() {
    mkdir -p "${LOG_DIR}"
    touch "${LOG_FILE}"
    chmod 644 "${LOG_FILE}"
}

log_write() {
    local level="$1"
    local message="$2"
    local timestamp
    timestamp=$(date '+%Y-%m-%d %H:%M:%S')
    local log_line="[${timestamp}] [${level}] ${message}"

    # 写入日志文件
    echo "${log_line}" >> "${LOG_FILE}"

    # 同时输出到控制台 (带颜色, 走 stderr 以免污染函数捕获 stdout)
    case "${level}" in
        INFO)  echo -e "${GREEN}[INFO]${NC} ${message}" >&2 ;;
        WARN)  echo -e "${YELLOW}[WARN]${NC} ${message}" >&2 ;;
        ERROR) echo -e "${RED}[ERROR]${NC} ${message}" >&2 ;;
        DEBUG) echo -e "${BLUE}[DEBUG]${NC} ${message}" >&2 ;;
        STEP)  echo -e "${CYAN}[STEP]${NC} ${message}" >&2 ;;
        *)     echo -e "${message}" >&2 ;;
    esac
}

log_separator() {
    local char="$1"
    local len="${2:-60}"
    local line
    line=$(printf "%${len}s" "" | tr " " "${char}")
    echo "${line}" >> "${LOG_FILE}"
    echo -e "${CYAN}${line}${NC}" >&2
}

# ============================ 工具函数 ============================

check_command() {
    command -v "$1" >/dev/null 2>&1
}

# 检测网络连通性 (支持 HTTP/HTTPS)
check_connectivity() {
    local url="$1"
    local timeout="${2:-${TIMEOUT_SECONDS}}"

    if check_command curl; then
        local http_code
        http_code=$(curl -s -o /dev/null -w "%{http_code}" \
            --connect-timeout "${timeout}" \
            --max-time "$((timeout + 5))" \
            -L "${url}" 2>/dev/null || echo "000")

        if [[ "${http_code}" =~ ^(200|301|302|304)$ ]]; then
            return 0
        else
            return 1
        fi
    elif check_command wget; then
        if wget --timeout="${timeout}" --tries=1 -q -O /dev/null "${url}" 2>/dev/null; then
            return 0
        else
            return 1
        fi
    else
        log_write "ERROR" "系统中未找到 curl 或 wget，无法检测网络连通性"
        return 1
    fi
}

# 检测 GitHub 官方连接
check_github_official() {
    log_write "STEP" "正在检测 GitHub 官方连接状态..."
    log_write "DEBUG" "检测目标: ${GITHUB_OFFICIAL}"

    local retry=0
    local success=false

    while [[ ${retry} -lt ${MAX_RETRIES} ]]; do
        if check_connectivity "${GITHUB_OFFICIAL}"; then
            success=true
            break
        fi
        retry=$((retry + 1))
        if [[ ${retry} -lt ${MAX_RETRIES} ]]; then
            log_write "WARN" "连接失败，${TIMEOUT_SECONDS}秒后重试 (${retry}/${MAX_RETRIES})..."
            sleep "${TIMEOUT_SECONDS}"
        fi
    done

    if [[ "${success}" == "true" ]]; then
        log_write "INFO" "✅ GitHub 官方连接正常"
        return 0
    else
        log_write "ERROR" "❌ GitHub 官方连接失败 (HTTP超时或无响应)"
        return 1
    fi
}

# 检测镜像站可用性
check_mirror() {
    local mirror_domain="$1"
    local mirror_type="$2"
    local mirror_desc="$3"

    local test_url
    if [[ "${mirror_type}" == "proxy" ]]; then
        # 代理型镜像站：测试代理 github 的能力
        test_url="https://${mirror_domain}/https://github.com"
    else
        # 完整镜像站：直接测试
        test_url="https://${mirror_domain}"
    fi

    log_write "DEBUG" "正在测试镜像站: ${mirror_domain} (${mirror_desc})"
    log_write "DEBUG" "测试URL: ${test_url}"

    if check_connectivity "${test_url}" 8; then
        log_write "INFO" "✅ 镜像站可用: ${mirror_domain} (${mirror_desc})"
        return 0
    else
        log_write "DEBUG" "❌ 镜像站不可用: ${mirror_domain}"
        return 1
    fi
}

# 查找可用镜像站
find_working_mirror() {
    log_write "STEP" "开始查找可用镜像站..."
    log_write "INFO" "候选镜像站数量: ${#MIRROR_CANDIDATES[@]}"

    local working_mirror=""
    local working_type=""
    local working_desc=""

    for candidate in "${MIRROR_CANDIDATES[@]}"; do
        IFS='|' read -r domain type desc <<< "${candidate}"

        if check_mirror "${domain}" "${type}" "${desc}"; then
            working_mirror="${domain}"
            working_type="${type}"
            working_desc="${desc}"
            break
        fi

        # 短暂延迟，避免对镜像站造成压力
        sleep 1
    done

    if [[ -n "${working_mirror}" ]]; then
        log_write "INFO" "找到可用镜像站: ${working_mirror}"
        log_write "INFO" "镜像类型: ${working_type} | ${working_desc}"
        echo "${working_mirror}|${working_type}"
        return 0
    else
        log_write "ERROR" "❌ 所有候选镜像站均不可用"
        return 1
    fi
}

# ============================ 环境变量配置 ============================

# 生成环境变量配置内容
generate_env_content() {
    local mirror="$1"
    local mtype="$2"

    local content=""
    content+="# ===== GitHub 镜像加速配置 (由 github_mirror_auto.sh 自动生成) =====
"
    content+="# 生成时间: $(date '+%Y-%m-%d %H:%M:%S')
"
    content+="# 当前镜像: ${mirror} (类型: ${mtype})
"
    content+="# 脚本路径: ${SCRIPT_DIR}/github_mirror_auto.sh
"
    content+="
"

    if [[ "${mtype}" == "full" ]]; then
        # 完整镜像站：替换域名
        content+="export GITHUB_MIRROR='https://${mirror}'
"
        content+="export GITHUB_OFFICIAL='https://github.com'
"
        content+="export GITHUB_API_MIRROR='https://${mirror}'
"
        content+="export GITHUB_RAW_MIRROR='https://${mirror}'
"
        content+="
"
        content+="# Git 全局配置：使用镜像站加速 clone
"
        content+="git config --global url.\"https://${mirror}\".insteadOf \"https://github.com\" 2>/dev/null || true
"
    else
        # 代理型镜像站：前缀代理
        content+="export GITHUB_MIRROR='https://${mirror}'
"
        content+="export GITHUB_OFFICIAL='https://github.com'
"
        content+="export GITHUB_API_MIRROR='https://${mirror}/https://api.github.com'
"
        content+="export GITHUB_RAW_MIRROR='https://${mirror}/https://raw.githubusercontent.com'
"
    fi

    # 通用辅助函数
    content+="
"
    content+="# 辅助函数：将 GitHub URL 转换为镜像 URL
"
    content+="github_mirror_url() {
"
    content+="    local url=\"\$1\"
"
    if [[ "${mtype}" == "full" ]]; then
        content+="    echo \"\${url/github.com/${mirror}}\"
"
    else
        content+="    echo \"https://${mirror}/\${url}\"
"
    fi
    content+="}
"
    content+="
"
    content+="# 辅助函数：快速 clone GitHub 仓库
"
    content+="ghclone() {
"
    content+="    local repo=\"\$1\"
"
    content+="    local target=\"\${2:-}\"
"
    if [[ "${mtype}" == "full" ]]; then
        content+="    local mirror_url=\"\${repo/github.com/${mirror}}\"
"
    else
        content+="    local mirror_url=\"https://${mirror}/\${repo}\"
"
    fi
    content+="    if [[ -n \"\${target}\" ]]; then
"
    content+="        git clone \"\${mirror_url}\" \"\${target}\"
"
    content+="    else
"
    content+="        git clone \"\${mirror_url}\"
"
    content+="    fi
"
    content+="}
"
    content+="
"
    content+="# ===== 配置结束 =====
"

    echo "${content}"
}

# 配置环境变量到 shell 配置文件
configure_env() {
    local mirror="$1"
    local mtype="$2"

    log_write "STEP" "正在配置环境变量..."
    log_write "INFO" "目标镜像: ${mirror} (类型: ${mtype})"

    local env_content
    env_content=$(generate_env_content "${mirror}" "${mtype}")

    # 创建临时环境变量文件
    local env_file="${SCRIPT_DIR}/.github_mirror_env"

    # 写入环境变量文件
    {
        echo "# GitHub Mirror Environment Variables"
        echo "# Generated: $(date '+%Y-%m-%d %H:%M:%S')"
        echo ""
        echo "export GITHUB_MIRROR='https://${mirror}'"
        echo "export GITHUB_MIRROR_TYPE='${mtype}'"
        echo "export GITHUB_MIRROR_SET_AT='$(date -Iseconds)'"
        echo ""
        echo "# Source this file: source ${env_file}"
    } > "${env_file}"

    log_write "INFO" "环境变量文件已创建: ${env_file}"

    # 尝试注入到 shell 配置文件
    local configured=false
    for shell_rc in "${ENV_FILES[@]}"; do
        if [[ -f "${shell_rc}" ]]; then
            # 检查是否已有配置
            if grep -q "github_mirror_auto.sh" "${shell_rc}" 2>/dev/null; then
                log_write "WARN" "检测到已有配置存在于 ${shell_rc}，跳过注入"
                continue
            fi

            # 添加 source 指令
            echo "" >> "${shell_rc}"
            echo "# >>> GitHub 镜像加速配置 (github_mirror_auto.sh) >>>" >> "${shell_rc}"
            echo "export GITHUB_MIRROR='https://${mirror}'" >> "${shell_rc}"
            echo "export GITHUB_MIRROR_TYPE='${mtype}'" >> "${shell_rc}"
            echo "[[ -f '${env_file}' ]] && source '${env_file}'" >> "${shell_rc}"
            echo "# <<< GitHub 镜像加速配置 <<<" >> "${shell_rc}"

            log_write "INFO" "✅ 已配置到: ${shell_rc}"
            configured=true
        fi
    done

    if [[ "${configured}" == "false" ]]; then
        log_write "WARN" "未找到合适的 shell 配置文件，请手动 source 环境变量文件"
        log_write "INFO" "手动加载命令: source ${env_file}"
    fi

    # 立即生效当前会话
    export GITHUB_MIRROR="https://${mirror}"
    export GITHUB_MIRROR_TYPE="${mtype}"

    log_write "INFO" "当前会话已设置 GITHUB_MIRROR=${GITHUB_MIRROR}"
}

# 配置 Git 全局镜像 (如果可用)
configure_git_mirror() {
    local mirror="$1"
    local mtype="$2"

    if ! check_command git; then
        log_write "WARN" "未检测到 git 命令，跳过 Git 配置"
        return 0
    fi

    log_write "STEP" "正在配置 Git 全局镜像..."

    if [[ "${mtype}" == "full" ]]; then
        # 完整镜像站：配置 insteadOf
        git config --global url."https://${mirror}".insteadOf "https://github.com" 2>/dev/null || {
            log_write "WARN" "Git insteadOf 配置失败"
        }

        # 同时配置 submodule 加速
        git config --global url."https://${mirror}".insteadOf "https://raw.githubusercontent.com" 2>/dev/null || true

        log_write "INFO" "✅ Git 全局镜像已配置: github.com -> ${mirror}"
    else
        # 代理型：配置 http.proxy (不推荐，仅作提示)
        log_write "WARN" "代理型镜像站不支持 Git insteadOf 配置，建议手动替换 clone URL"
        log_write "INFO" "示例: git clone https://${mirror}/https://github.com/user/repo.git"
    fi

    # 显示当前 Git 配置
    log_write "DEBUG" "当前 Git URL 替换规则:"
    git config --global --get-regexp "url.*insteadOf" 2>/dev/null | while read -r line; do
        log_write "DEBUG" "  ${line}"
    done || true
}

# 清理旧的 Git 配置
cleanup_git_config() {
    log_write "STEP" "清理旧的 Git 镜像配置..."

    if check_command git; then
        # 移除所有旧的 insteadOf 配置
        git config --global --unset-all url."https://github.com".insteadOf 2>/dev/null || true

        # 移除已知旧镜像站的配置
        local old_mirrors=("gitclone.com" "kkgithub.com" "bgithub.xyz" "kgithub.com" "gh-proxy.com")
        for old in "${old_mirrors[@]}"; do
            git config --global --unset-all url."https://${old}".insteadOf 2>/dev/null || true
        done

        log_write "INFO" "✅ 旧配置已清理"
    fi
}

# ============================ 状态检测 ============================

# 检测当前环境变量状态
check_current_status() {
    log_write "STEP" "检测当前环境状态..."

    log_write "INFO" "当前 GITHUB_MIRROR: ${GITHUB_MIRROR:-未设置}"
    log_write "INFO" "当前 GITHUB_MIRROR_TYPE: ${GITHUB_MIRROR_TYPE:-未设置}"

    if check_command git; then
        local git_mirror
        git_mirror=$(git config --global --get-regexp "url.*insteadOf" 2>/dev/null | head -1 || echo "未配置")
        log_write "INFO" "当前 Git 镜像规则: ${git_mirror}"
    fi
}

# 测试镜像站实际效果
test_mirror_effectiveness() {
    local mirror="$1"
    local mtype="$2"

    log_write "STEP" "测试镜像站实际效果..."

    local test_repo="https://github.com/octocat/Hello-World"
    local mirror_url

    if [[ "${mtype}" == "full" ]]; then
        mirror_url="${test_repo/github.com/${mirror}}"
    else
        mirror_url="https://${mirror}/${test_repo}"
    fi

    log_write "DEBUG" "测试URL: ${mirror_url}"

    local start_time end_time duration
    start_time=$(date +%s%3N)

    if check_connectivity "${mirror_url}" 15; then
        end_time=$(date +%s%3N)
        duration=$((end_time - start_time))
        log_write "INFO" "✅ 镜像站响应正常，延迟: ${duration}ms"
        return 0
    else
        log_write "ERROR" "❌ 镜像站响应异常"
        return 1
    fi
}

# ============================ 主流程 ============================

show_banner() {
    echo ""
    log_separator "="
    log_write "INFO" "GitHub 镜像自动检测与配置脚本"
    log_write "INFO" "版本: 1.0.0 | 日期: $(date '+%Y-%m-%d %H:%M:%S')"
    log_write "INFO" "日志文件: ${LOG_FILE}"
    log_separator "="
    echo ""
}

show_help() {
    cat << EOF
用法: $0 [选项]

选项:
    -h, --help          显示帮助信息
    -c, --check         仅检测 GitHub 连接状态，不配置镜像
    -r, --reset         重置配置，恢复官方 GitHub 连接
    -s, --status        显示当前配置状态
    -d, --daemon        守护模式：后台定期检查并自动切换
    -l, --list          列出所有候选镜像站
    -t, --test          测试当前配置的镜像站效果

示例:
    $0                  执行完整检测与配置流程
    $0 --check          仅检测连接状态
    $0 --reset          清除所有镜像配置
    $0 --daemon         后台守护模式运行

环境变量:
    GITHUB_MIRROR       当前使用的镜像站地址
    GITHUB_MIRROR_TYPE  镜像类型 (full/proxy)

EOF
}

# 列出所有候选镜像站
list_mirrors() {
    log_write "STEP" "候选镜像站列表"
    log_separator "-"
    printf "%-25s %-10s %s\n" "域名" "类型" "描述"
    log_separator "-"

    for candidate in "${MIRROR_CANDIDATES[@]}"; do
        IFS='|' read -r domain type desc <<< "${candidate}"
        local type_str
        if [[ "${type}" == "full" ]]; then
            type_str="完整镜像"
        else
            type_str="文件代理"
        fi
        printf "%-25s %-10s %s\n" "${domain}" "${type_str}" "${desc}"
    done
    log_separator "-"
}

# 重置配置
reset_config() {
    log_write "STEP" "正在重置所有配置..."

    # 清理环境变量
    unset GITHUB_MIRROR 2>/dev/null || true
    unset GITHUB_MIRROR_TYPE 2>/dev/null || true

    # 清理 Git 配置
    cleanup_git_config

    # 从 shell 配置文件中移除
    for shell_rc in "${ENV_FILES[@]}"; do
        if [[ -f "${shell_rc}" ]]; then
            # 使用 sed 移除标记之间的内容
            sed -i '/# >>> GitHub 镜像加速配置/,/# <<< GitHub 镜像加速配置/d' "${shell_rc}" 2>/dev/null || \
            sed -i '' '/# >>> GitHub 镜像加速配置/,/# <<< GitHub 镜像加速配置/d' "${shell_rc}" 2>/dev/null || true
            log_write "INFO" "已清理: ${shell_rc}"
        fi
    done

    # 删除环境变量文件
    rm -f "${SCRIPT_DIR}/.github_mirror_env"

    log_write "INFO" "✅ 所有配置已重置，恢复使用官方 GitHub"
}

# 守护模式
daemon_mode() {
    log_write "STEP" "进入守护模式 (检查间隔: ${CHECK_INTERVAL}秒)"

    # 写入 PID 文件
    echo $$ > "${PID_FILE}"

    # 捕获退出信号
    trap 'rm -f "${PID_FILE}"; log_write "INFO" "守护进程已退出"; exit 0' EXIT INT TERM

    while true; do
        log_separator "-"
        log_write "INFO" "守护进程检查周期: $(date '+%Y-%m-%d %H:%M:%S')"

        if ! check_github_official; then
            log_write "WARN" "GitHub 官方连接异常，尝试切换镜像..."

            local mirror_info
            if mirror_info=$(find_working_mirror); then
                IFS='|' read -r mirror_domain mirror_type <<< "${mirror_info}"
                configure_env "${mirror_domain}" "${mirror_type}"
                configure_git_mirror "${mirror_domain}" "${mirror_type}"
                test_mirror_effectiveness "${mirror_domain}" "${mirror_type}"
            else
                log_write "ERROR" "未找到可用镜像站，将在下次检查时重试"
            fi
        else
            log_write "INFO" "GitHub 官方连接正常，无需切换"

            # 如果当前配置了镜像，检查是否需要恢复官方
            if [[ -n "${GITHUB_MIRROR:-}" ]]; then
                log_write "INFO" "当前使用镜像: ${GITHUB_MIRROR}，官方已恢复，建议手动重置"
            fi
        fi

        log_write "INFO" "下次检查: ${CHECK_INTERVAL}秒后"
        sleep "${CHECK_INTERVAL}"
    done
}

# 主执行流程
main() {
    # 初始化日志
    log_init

    # 解析参数
    case "${1:-}" in
        -h|--help)
            show_help
            exit 0
            ;;
        -c|--check)
            show_banner
            check_current_status
            if check_github_official; then
                log_write "INFO" "GitHub 官方连接正常，无需配置镜像"
                exit 0
            else
                log_write "ERROR" "GitHub 官方连接失败"
                exit 1
            fi
            ;;
        -r|--reset)
            show_banner
            reset_config
            exit 0
            ;;
        -s|--status)
            show_banner
            check_current_status
            exit 0
            ;;
        -d|--daemon)
            show_banner
            daemon_mode
            exit 0
            ;;
        -l|--list)
            list_mirrors
            exit 0
            ;;
        -t|--test)
            show_banner
            if [[ -n "${GITHUB_MIRROR:-}" ]]; then
                test_mirror_effectiveness "${GITHUB_MIRROR}" "${GITHUB_MIRROR_TYPE:-full}"
            else
                log_write "ERROR" "未配置镜像站，请先运行脚本进行配置"
                exit 1
            fi
            exit 0
            ;;
        "")
            : # 继续执行主流程
            ;;
        *)
            echo "未知选项: $1"
            show_help
            exit 1
            ;;
    esac

    # 显示横幅
    show_banner

    # 检测当前状态
    check_current_status

    # 步骤1: 检测 GitHub 官方连接
    log_separator "="
    log_write "STEP" "【步骤 1/4】检测 GitHub 官方连接"
    log_separator "="

    if check_github_official; then
        log_write "INFO" "GitHub 官方连接正常，无需配置镜像"
        log_write "INFO" "如需强制配置镜像，请使用 --reset 后重新运行"
        exit 0
    fi

    # 步骤2: 查找可用镜像站
    log_separator "="
    log_write "STEP" "【步骤 2/4】查找可用镜像站"
    log_separator "="

    local mirror_info
    if ! mirror_info=$(find_working_mirror); then
        log_write "ERROR" "无法找到可用的 GitHub 镜像站"
        log_write "ERROR" "建议: 检查网络连接，或稍后重试"
        exit 1
    fi

    IFS='|' read -r mirror_domain mirror_type <<< "${mirror_info}"

    # 步骤3: 配置环境变量
    log_separator "="
    log_write "STEP" "【步骤 3/4】配置环境变量与 Git"
    log_separator "="

    configure_env "${mirror_domain}" "${mirror_type}"
    configure_git_mirror "${mirror_domain}" "${mirror_type}"

    # 步骤4: 验证配置效果
    log_separator "="
    log_write "STEP" "【步骤 4/4】验证配置效果"
    log_separator "="

    if test_mirror_effectiveness "${mirror_domain}" "${mirror_type}"; then
        log_write "INFO" "✅ 配置完成！GitHub 镜像已生效"
        log_write "INFO" "镜像地址: https://${mirror_domain}"
        log_write "INFO" "镜像类型: ${mirror_type}"
        log_write "INFO" ""
        log_write "INFO" "使用提示:"
        log_write "INFO" "  1. 环境变量 GITHUB_MIRROR 已设置"
        log_write "INFO" "  2. 新开终端窗口将自动加载配置"
        log_write "INFO" "  3. 当前终端可执行: source ${SCRIPT_DIR}/.github_mirror_env"
        log_write "INFO" "  4. Git clone 已自动走镜像加速"
        log_write "INFO" "  5. 查看状态: $0 --status"
        log_write "INFO" "  6. 重置配置: $0 --reset"
    else
        log_write "WARN" "⚠️ 配置已应用，但镜像站测试未通过"
        log_write "WARN" "镜像站可能不稳定，建议稍后重试或更换镜像"
    fi

    log_separator "="
    log_write "INFO" "脚本执行完成，详细日志: ${LOG_FILE}"
    log_separator "="
}

# 执行主函数
main "$@"

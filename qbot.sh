#!/usr/bin/env bash
set -euo pipefail

# qq-maid-bot 管理脚本
# 部署: bash /root/qbot.sh deploy

DEFAULT_APP_DIR="/root/qq-maid-bot"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        # Git Bash/MSYS2/Cygwin 没有通用的 /root，默认安装到当前 Windows 用户目录。
        DEFAULT_APP_DIR="${HOME}/qq-maid-bot"
        ;;
esac
APP_DIR="${QBOT_APP_DIR:-${DEFAULT_APP_DIR}}"
REPO_SLUG="${QBOT_REPO_SLUG:-kuliantnt/qq-maid-bot}"
RELEASES_URL="https://github.com/${REPO_SLUG}/releases"
API_LATEST_URL="https://api.github.com/repos/${REPO_SLUG}/releases/latest"
GITHUB_ACCEL_PROXY="${QBOT_GITHUB_PROXY:-${GH_PROXY:-}}"
GITHUB_ACCEL_PROXIES="${QBOT_GITHUB_PROXIES:-}"
GITHUB_PROBE_CONNECT_TIMEOUT="${QBOT_GITHUB_PROBE_CONNECT_TIMEOUT:-5}"
GITHUB_PROBE_MAX_TIME="${QBOT_GITHUB_PROBE_MAX_TIME:-12}"
GITHUB_DOWNLOAD_CONNECT_TIMEOUT="${QBOT_GITHUB_DOWNLOAD_CONNECT_TIMEOUT:-10}"
GITHUB_DOWNLOAD_MAX_TIME="${QBOT_GITHUB_DOWNLOAD_MAX_TIME:-300}"
CURL_PROXY="${QBOT_CURL_PROXY:-}"
SELF="${BASH_SOURCE[0]}"
TMP_DIR_TO_CLEAN=""
UI_RESET=""
UI_BOLD=""
UI_DIM=""
UI_RED=""
UI_GREEN=""
UI_YELLOW=""
UI_BLUE=""
UI_CYAN=""

cleanup_tmp_dir() {
    if [[ -n "${TMP_DIR_TO_CLEAN}" && -d "${TMP_DIR_TO_CLEAN}" ]]; then
        rm -rf "${TMP_DIR_TO_CLEAN}"
    fi
}
trap cleanup_tmp_dir EXIT

init_ui() {
    local colors

    [[ -z "${NO_COLOR:-}" && -t 2 ]] || return 0
    command -v tput >/dev/null 2>&1 || return 0
    colors="$(tput colors 2>/dev/null || echo 0)"
    [[ "${colors}" =~ ^[0-9]+$ ]] || colors=0
    ((colors >= 8)) || return 0

    UI_RESET="$(tput sgr0)"
    UI_BOLD="$(tput bold)"
    UI_DIM="$(tput dim 2>/dev/null || true)"
    UI_RED="$(tput setaf 1)"
    UI_GREEN="$(tput setaf 2)"
    UI_YELLOW="$(tput setaf 3)"
    UI_BLUE="$(tput setaf 4)"
    UI_CYAN="$(tput setaf 6)"
}

ui_clear_screen() {
    [[ "${QBOT_NO_CLEAR:-0}" != "1" && -t 1 ]] || return 0
    if command -v clear >/dev/null 2>&1; then
        clear
    else
        printf '\033[H\033[2J'
    fi
}

ui_err() {
    printf '%b\n' "$*" >&2
}

ui_header() {
    ui_err "${UI_BOLD}${UI_CYAN}$*${UI_RESET}"
}

ui_note() {
    ui_err "${UI_DIM}$*${UI_RESET}"
}

ui_warn() {
    ui_err "${UI_YELLOW}$*${UI_RESET}"
}

ui_fail() {
    ui_err "${UI_RED}$*${UI_RESET}"
}

ui_out_status() {
    local color="$1"
    local label="$2"
    local text="$3"

    if [[ -n "${UI_RESET}" && -t 1 ]]; then
        printf '%b%s%b %s\n' "${color}" "${label}" "${UI_RESET}" "${text}"
    else
        printf '%s %s\n' "${label}" "${text}"
    fi
}

usage() {
    cat <<EOF
用法:
  qbot start                  启动 qq-maid-bot
  qbot stop                   停止 qq-maid-bot
  qbot restart                重启 qq-maid-bot
  qbot status                 查看状态
  qbot log                    查看并跟随日志
  qbot health                 请求 /healthz
  qbot console                查看控制台 URL 状态
  qbot install [version]      从 GitHub Releases 下载并安装，默认 latest
  qbot update [version]       匹配版本号后更新，默认 latest
  qbot patch [version]        update 的别名
  qbot version                查看本地版本与最新版本
  qbot config show [KEY...]   查看配置（默认脱敏）
  qbot config get KEY         读取单个配置值
  qbot config set KEY=VALUE   写入 config/.env
  qbot config bot ...         配置 QQ Bot 信息
  qbot config ai ...          配置 AI 渠道与模型，交互模式会从接口获取模型列表
  qbot deploy                 将本脚本安装为系统命令 (默认 /usr/local/bin/qbot)

常用配置:
  qbot config bot --app-id 123 --app-secret xxx --sandbox false
  qbot config ai --provider openai --api-key sk-xxx --model gpt-5.6-luna
  qbot config ai --provider auto --api-key sk-xxx --base-url https://你的兼容网关 --model openai:gpt-5.6-luna
  qbot config ai --provider deepseek --api-key sk-xxx --model deepseek-chat
  qbot config ai --provider mimo --api-key xxx --model mimo-v2.5-pro
  qbot config set LLM_MODEL=openai:gpt-5.6-luna

目录: ${APP_DIR}
项目: https://github.com/${REPO_SLUG}
下载: 默认直连官方 GitHub；如需加速，可用 QBOT_GITHUB_PROXY 或 QBOT_GITHUB_PROXIES 指定可信镜像
EOF
}

die() {
    ui_fail "错误: $*"
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "未找到命令: $1"
}

curl_qbot() {
    local args=()
    if [[ -n "${CURL_PROXY}" ]]; then
        args+=(--proxy "${CURL_PROXY}")
    fi

    curl "${args[@]}" "$@"
}

github_accel_prefixes() {
    echo ""

    {
        if [[ -n "${GITHUB_ACCEL_PROXY}" ]]; then
            echo "${GITHUB_ACCEL_PROXY%/}/"
        fi

        if [[ -n "${GITHUB_ACCEL_PROXIES}" ]]; then
            printf '%s\n' ${GITHUB_ACCEL_PROXIES}
        fi
    } | awk 'NF {sub(/\/?$/, "/", $0); if (!seen[$0]++) print}'
}

github_url_for_prefix() {
    local prefix="$1"
    local raw_url="$2"

    if [[ -n "${prefix}" ]]; then
        echo "${prefix%/}/${raw_url}"
    else
        echo "${raw_url}"
    fi
}

github_prefix_label() {
    local prefix="$1"

    if [[ -n "${prefix}" ]]; then
        echo "${prefix%/}/"
    else
        echo "直连 GitHub"
    fi
}

probe_github_prefix_ms() {
    local prefix="$1"
    local raw_url="$2"
    local url result http_code total

    url="$(github_url_for_prefix "${prefix}" "${raw_url}")"
    result="$(
        curl_qbot -L -sS --range 0-0 \
            --connect-timeout "${GITHUB_PROBE_CONNECT_TIMEOUT}" \
            --max-time "${GITHUB_PROBE_MAX_TIME}" \
            -o /dev/null \
            -w '%{http_code} %{time_total}' \
            "${url}" 2>/dev/null || true
    )"

    http_code="${result%% *}"
    total="${result#* }"
    if [[ "${http_code}" =~ ^20[0-9]$ || "${http_code}" =~ ^30[0-9]$ ]]; then
        awk -v sec="${total}" 'BEGIN { printf "%d", sec * 1000 }'
    else
        echo 999999
    fi
}

sorted_github_sources() {
    local raw_url="$1"
    local tmp_file prefix latency order token

    tmp_file="$(mktemp)"
    order=0

    while IFS= read -r prefix; do
        latency="$(probe_github_prefix_ms "${prefix}" "${raw_url}")"
        token="${prefix:-__DIRECT__}"
        if [[ "${latency}" -lt 999999 ]]; then
            echo "可用 GitHub 源: $(github_prefix_label "${prefix}") (${latency}ms)" >&2
        else
            echo "跳过不可用源: $(github_prefix_label "${prefix}")" >&2
        fi
        printf '%s\t%s\t%s\n' "${latency}" "${order}" "${token}" >> "${tmp_file}"
        order=$((order + 1))
    done < <(github_accel_prefixes)

    sort -n -k1,1 -k2,2 "${tmp_file}" | awk -F '\t' '$1 < 999999 {print $1 "\t" $3}'
    rm -f "${tmp_file}"
}

downloaded_file_is_valid() {
    local file="$1"
    local description="$2"

    [[ -s "${file}" ]] || return 1

    case "${description}" in
        *.tar.gz)
            gzip -t "${file}" >/dev/null 2>&1
            ;;
        *.zip)
            unzip -tq "${file}" >/dev/null 2>&1
            ;;
        *.sha256)
            grep -Eq '^[[:xdigit:]]{64}[[:space:]]' "${file}"
            ;;
        *)
            return 0
            ;;
    esac
}

download_github_file() {
    local raw_url="$1"
    local output="$2"
    local description="$3"
    local latency prefix_token prefix url

    rm -f "${output}"
    echo "测速 GitHub 下载源: ${description}" >&2

    while IFS=$'\t' read -r latency prefix_token; do
        prefix="${prefix_token}"
        [[ "${prefix}" == "__DIRECT__" ]] && prefix=""
        url="$(github_url_for_prefix "${prefix}" "${raw_url}")"
        echo "尝试下载源: $(github_prefix_label "${prefix}") (${latency}ms)" >&2

        rm -f "${output}"
        if curl_qbot -fL --retry 2 \
            --connect-timeout "${GITHUB_DOWNLOAD_CONNECT_TIMEOUT}" \
            --max-time "${GITHUB_DOWNLOAD_MAX_TIME}" \
            -o "${output}" "${url}"; then
            if downloaded_file_is_valid "${output}" "${description}"; then
                return 0
            fi
            echo "下载结果无效，继续尝试下一个源: $(github_prefix_label "${prefix}")" >&2
        fi
    done < <(sorted_github_sources "${raw_url}")

    echo "所有 GitHub 下载源失败，最后重试官方直连: ${description}" >&2
    rm -f "${output}"
    if curl_qbot -fL --retry 2 \
        --connect-timeout "${GITHUB_DOWNLOAD_CONNECT_TIMEOUT}" \
        --max-time "${GITHUB_DOWNLOAD_MAX_TIME}" \
        -o "${output}" "${raw_url}"; then
        if downloaded_file_is_valid "${output}" "${description}"; then
            return 0
        fi
    fi

    die "下载失败: ${description}"
}

install_deps() {
    local missing=()
    local required=(curl sha256sum mktemp)
    case "$(uname -s)" in
        MINGW*|MSYS*|CYGWIN*) required+=(unzip) ;;
        *) required+=(tar gzip) ;;
    esac

    for cmd in "${required[@]}"; do
        command -v "${cmd}" >/dev/null 2>&1 || missing+=("${cmd}")
    done

    ((${#missing[@]} == 0)) && return 0

    echo "安装依赖: ${missing[*]}"
    case "$(uname -s)" in
        MINGW*|MSYS*)
            if command -v pacman >/dev/null 2>&1; then
                local packages=()
                local missing_cmd package
                for missing_cmd in "${missing[@]}"; do
                    case "${missing_cmd}" in
                        curl) package="curl" ;;
                        unzip) package="unzip" ;;
                        sha256sum|mktemp) package="coreutils" ;;
                        *) die "MSYS2 无法自动匹配依赖命令: ${missing_cmd}" ;;
                    esac
                    [[ " ${packages[*]} " == *" ${package} "* ]] || packages+=("${package}")
                done
                # --needed 避免重复安装已有包；这里只安装缺失命令对应的最小包集合。
                pacman -S --needed --noconfirm "${packages[@]}"
                hash -r
                for missing_cmd in "${missing[@]}"; do
                    command -v "${missing_cmd}" >/dev/null 2>&1 || die "pacman 执行后仍缺少命令: ${missing_cmd}"
                done
                return 0
            fi
            die "缺少命令: ${missing[*]}。当前 Shell 未找到 pacman；Git Bash 请通过安装器补齐依赖，MSYS2 请确认 pacman 可用"
            ;;
        CYGWIN*)
            die "缺少命令: ${missing[*]}。Cygwin 请通过 setup-x86_64.exe 安装 curl、unzip 和 coreutils"
            ;;
    esac

    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -qq
        apt-get install -y curl ca-certificates tar gzip coreutils
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y curl ca-certificates tar gzip coreutils
    else
        die "请先手动安装缺少的命令: ${missing[*]}"
    fi
}

detect_target() {
    local os arch machine
    case "$(uname -s)" in
        Linux)
            os="linux"
            ;;
        Darwin)
            os="macos"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            # Windows Release 在 Git Bash、MSYS2 和 Cygwin 中统一使用 MSVC x86_64 包。
            os="windows"
            ;;
        *)
            die "当前系统暂不支持自动匹配 Release 包: $(uname -s)"
            ;;
    esac

    machine="$(uname -m)"
    if [[ "${os}" == "windows" ]]; then
        case "${machine}" in
            x86_64|amd64)
                # Release 矩阵目前只发布 Windows x86_64，禁止拼出不存在的 windows-aarch64。
                echo "windows-x86_64"
                return 0
                ;;
            aarch64|arm64)
                die "当前不提供 Windows ARM64 Release；请使用 x86_64 Windows Shell，或在 WSL 中安装 Linux Release"
                ;;
            *)
                die "当前 Windows 架构暂不支持自动匹配 Release 包: ${machine}"
                ;;
        esac
    fi

    case "${machine}" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            die "当前架构暂不支持自动匹配 Release 包: ${machine}"
            ;;
    esac

    echo "${os}-${arch}"
}

normalize_version() {
    local version="${1:-latest}"
    if [[ -z "${version}" || "${version}" == "latest" ]]; then
        echo "latest"
    elif [[ "${version}" == v* ]]; then
        echo "${version}"
    else
        echo "v${version}"
    fi
}

latest_version() {
    need_cmd curl

    local tag effective_url
    tag="$(
        curl_qbot -fsSL --retry 2 --connect-timeout 10 "${API_LATEST_URL}" |
            sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' |
            head -n 1
    )"

    if [[ -z "${tag}" ]]; then
        effective_url="$(curl_qbot -fsSLI --retry 2 --connect-timeout 10 -o /dev/null -w '%{url_effective}' "${RELEASES_URL}/latest")"
        tag="${effective_url##*/}"
    fi

    [[ "${tag}" == v* ]] || die "无法解析最新 Release 版本号"
    echo "${tag}"
}

resolve_version() {
    local requested
    requested="$(normalize_version "${1:-latest}")"
    if [[ "${requested}" == "latest" ]]; then
        latest_version
    else
        echo "${requested}"
    fi
}

local_version() {
    if [[ -f "${APP_DIR}/VERSION" ]]; then
        tr -d '[:space:]' < "${APP_DIR}/VERSION"
        return 0
    fi

    return 1
}

read_qbot_pid() {
    local pid_file="${APP_DIR}/run/qq-maid-bot.pid"
    [[ -f "${pid_file}" ]] || return 1

    local pid
    pid="$(tr -d '[:space:]' < "${pid_file}")"
    [[ "${pid}" =~ ^[0-9]+$ ]] || return 1
    echo "${pid}"
}

is_pid_running() {
    local pid="${1:-}"
    [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null
}

is_qbot_running() {
    local pid
    pid="$(read_qbot_pid 2>/dev/null)" || return 1
    is_pid_running "${pid}"
}

require_installed() {
    [[ -x "${APP_DIR}/botctl.sh" ]] || die "未找到 ${APP_DIR}/botctl.sh，请先执行 qbot install"
}

run_botctl() {
    require_installed
    QQ_MAID_RUNTIME_DIR="${APP_DIR}" "${APP_DIR}/botctl.sh" "$@"
}

config_usage() {
    cat <<EOF
用法:
  qbot config show [KEY...]              查看配置（show 默认脱敏）
  qbot config get KEY                    输出单个配置原值
  qbot config path                       输出 config/.env 路径
  qbot config set KEY=VALUE [KEY=VALUE]  写入任意环境变量

  qbot config bot                         交互式配置 QQ Bot 信息
  qbot config bot --app-id ID --app-secret SECRET [--sandbox true|false]
                  [--display-name 名称] [--group-mode off|command|mention|active]
                  [--active-keywords 关键词] [--mention-ids IDS]

	  qbot config ai                          交互式配置 AI 渠道，并从接口获取一次模型列表后本地筛选
	                                        模型列表默认显示前 20 个，输入时实时筛选
	  qbot config ai --provider openai|deepseek|bigmodel|mimo|auto
                 [--api-key KEY] [--base-url URL] [--model MODEL]
                 [--private-model MODEL] [--group-model MODEL]
                 [--search-model MODEL] [--api-mode auto|chat_only]

示例:
  qbot config bot --app-id 1020xxxx --app-secret xxxxxx --sandbox false
  qbot config ai --provider openai --api-key sk-xxx --model gpt-5.6-luna
  qbot config ai --provider auto --api-key sk-xxx --base-url https://你的兼容网关 --model openai:gpt-5.6-luna
  qbot config ai --provider deepseek --api-key sk-xxx --model deepseek-chat
  qbot config ai --provider mimo --api-key xxx --model mimo-v2.5-pro
  qbot config set PRIVATE_LLM_MODEL=openai:gpt-5.6-luna
EOF
}

config_env_file() {
    echo "${APP_DIR}/config/.env"
}

ensure_config_env_file() {
    local file example legacy_example
    file="$(config_env_file)"
    example="${APP_DIR}/config/.env.example"
    legacy_example="${APP_DIR}/.env.example"

    mkdir -p "$(dirname -- "${file}")"
    if [[ ! -f "${file}" ]]; then
        if [[ -f "${example}" ]]; then
            cp -n "${example}" "${file}"
        elif [[ -f "${legacy_example}" ]]; then
            cp -n "${legacy_example}" "${file}"
        else
            : > "${file}"
        fi
        ui_warn "已创建配置文件: ${file}"
    fi

    echo "${file}"
}

CONFIG_BACKUP_CREATED=""
backup_config_file_once() {
    local file="$1"
    local backup

    [[ "${QBOT_CONFIG_NO_BACKUP:-0}" == "1" ]] && return 0
    [[ "${CONFIG_BACKUP_CREATED}" == "${file}" ]] && return 0

    backup="${file}.bak.$(date +%Y%m%d_%H%M%S)"
    cp -a "${file}" "${backup}"
    CONFIG_BACKUP_CREATED="${file}"
    ui_note "已备份配置: ${backup}"
}

validate_env_key() {
    local key="$1"
    [[ "${key}" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "非法配置名: ${key}"
}

validate_no_newline() {
    local value="$1"
    [[ "${value}" != *$'\n'* && "${value}" != *$'\r'* ]] || die "配置值不能包含换行"
}

shell_quote_env_value() {
    local value="$1"

    printf "'"
    while [[ "${value}" == *"'"* ]]; do
        printf "%s'\\\\''" "${value%%\'*}"
        value="${value#*\'}"
    done
    printf "%s'" "${value}"
}

decode_env_value() {
    local value="$1"
    local inner

    if [[ "${value}" == \'*\' && "${#value}" -ge 2 ]]; then
        inner="${value:1:${#value}-2}"
        printf '%s' "${inner}" | sed "s/'\\\\''/'/g"
    else
        printf '%s' "${value}"
    fi
}

set_env_var() {
    local key="$1"
    local value="$2"
    local file tmp owner group mode quoted_value

    validate_env_key "${key}"
    validate_no_newline "${value}"

    file="$(ensure_config_env_file)"
    backup_config_file_once "${file}"
    tmp="${file}.tmp.$$"
    owner="$(stat -c '%u' "${file}" 2>/dev/null || echo "")"
    group="$(stat -c '%g' "${file}" 2>/dev/null || echo "")"
    mode="$(stat -c '%a' "${file}" 2>/dev/null || echo "")"
    quoted_value="$(shell_quote_env_value "${value}")"

    QBOT_AWK_ENV_VALUE="${quoted_value}" awk -v key="${key}" '
        BEGIN {
            value = ENVIRON["QBOT_AWK_ENV_VALUE"]
            done = 0
        }
        $0 ~ "^[[:space:]]*" key "=" {
            if (!done) {
                print key "=" value
                done = 1
            }
            next
        }
        { print }
        END {
            if (!done) {
                print key "=" value
            }
        }
    ' "${file}" > "${tmp}"

    mv "${tmp}" "${file}"
    [[ -n "${owner}" && -n "${group}" ]] && chown "${owner}:${group}" "${file}" 2>/dev/null || true
    [[ -n "${mode}" ]] && chmod "${mode}" "${file}" 2>/dev/null || true
}

get_env_var() {
    local key="$1"
    local file
    local raw_value

    validate_env_key "${key}"
    file="$(ensure_config_env_file)"
    raw_value="$(
        awk -v key="${key}" '
        $0 ~ "^[[:space:]]*" key "=" {
            line = $0
            sub("^[[:space:]]*" key "=", "", line)
            value = line
            found = 1
        }
        END {
            if (found) print value
        }
    ' "${file}"
    )"
    decode_env_value "${raw_value}"
}

get_real_env_var() {
    local value

    value="$(get_env_var "$1")"
    case "${value}" in
        你的*|your*|YOUR*)
            echo ""
            ;;
        *)
            echo "${value}"
            ;;
    esac
}

mask_config_value() {
    local key="$1"
    local value="$2"
    local len

    if [[ -z "${value}" ]]; then
        echo ""
    elif [[ "${key}" =~ (API_KEY|SECRET|TOKEN|PASSWORD|_KEY$) ]]; then
        len="${#value}"
        if ((len <= 8)); then
            echo "********"
        else
            echo "${value:0:4}...${value: -4}"
        fi
    else
        echo "${value}"
    fi
}

prompt_display_default() {
    local key="$1"
    local value="$2"

    if [[ -z "${value}" ]]; then
        echo "未配置"
    else
        mask_config_value "${key}" "${value}"
    fi
}

config_done_hint() {
    local file
    file="$(config_env_file)"
    if [[ -n "${UI_RESET}" && -t 1 ]]; then
        printf '\n%b配置已写入%b %s\n' "${UI_GREEN}${UI_BOLD}" "${UI_RESET}" "${UI_CYAN}${file}${UI_RESET}"
    else
        printf '\n配置已写入: %s\n' "${file}"
    fi
    if is_qbot_running; then
        ui_out_status "${UI_YELLOW}" "提示:" "qbot 正在运行，执行 qbot restart 后生效"
    else
        ui_out_status "${UI_YELLOW}" "提示:" "下次 qbot start 时生效"
    fi
}

normalize_bool_value() {
    local value="$1"
    case "${value}" in
        true|false)
            echo "${value}"
            ;;
        1|yes|y|on)
            echo "true"
            ;;
        0|no|n|off)
            echo "false"
            ;;
        *)
            die "布尔值只能是 true/false"
            ;;
    esac
}

normalize_model_value() {
    local provider="$1"
    local model="$2"

    [[ -n "${model}" ]] || return 0

    if [[ "${model}" == *:* || "${model}" == *,* || "${provider}" == "auto" ]]; then
        if [[ "${provider}" == "auto" && "${model}" != *:* && "${model}" != *,* ]]; then
            echo "openai:${model}"
        else
            echo "${model}"
        fi
    else
        echo "${provider}:${model}"
    fi
}

normalize_base_url_value() {
    local provider="$1"
    local url="$2"

    url="$(printf '%s' "${url}" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
    [[ -n "${url}" ]] || return 0

    while [[ "${url}" == */ ]]; do
        url="${url%/}"
    done

    case "${provider}" in
        openai|auto)
            if [[ "${url}" =~ /v[0-9]+$ ]]; then
                echo "${url}"
            else
                echo "${url}/v1"
            fi
            ;;
        *)
            echo "${url}"
            ;;
    esac
}

provider_default_base_url() {
    case "$1" in
        openai|auto)
            echo "https://api.openai.com/v1"
            ;;
        deepseek)
            echo "https://api.deepseek.com"
            ;;
        bigmodel)
            echo "https://open.bigmodel.cn/api/paas/v4"
            ;;
        mimo)
            echo "https://api.xiaomimimo.com/v1"
            ;;
        *)
            die "不支持的 provider: $1"
            ;;
    esac
}

effective_model_base_url() {
    local provider="$1"
    local base_url="$2"

    if [[ -z "${base_url}" ]]; then
        provider_default_base_url "${provider}"
    else
        normalize_base_url_value "${provider}" "${base_url}"
    fi
}

fetch_provider_models() {
    local provider="$1"
    local api_key="$2"
    local base_url="$3"
    local endpoint body

    [[ -n "${api_key}" ]] || return 1

    base_url="$(effective_model_base_url "${provider}" "${base_url}")"
    endpoint="${base_url%/}/models"

    body="$(
        curl_qbot -fsSL \
            --connect-timeout "${QBOT_MODEL_LIST_CONNECT_TIMEOUT:-8}" \
            --max-time "${QBOT_MODEL_LIST_MAX_TIME:-25}" \
            -H "Authorization: Bearer ${api_key}" \
            "${endpoint}" 2>/dev/null || true
    )"
    [[ -n "${body}" ]] || return 1

    printf '%s' "${body}" |
        sed 's/"id"/\
"id"/g' |
        sed -nE 's/.*"id"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' |
        awk 'NF && !seen[$0]++'
}

provider_key_var() {
    case "$1" in
        openai)
            echo "OPENAI_API_KEY"
            ;;
        deepseek)
            echo "DEEPSEEK_API_KEY"
            ;;
        bigmodel)
            echo "BIGMODEL_API_KEY"
            ;;
        mimo)
            echo "MIMO_API_KEY"
            ;;
        auto)
            echo "OPENAI_API_KEY"
            ;;
        *)
            die "不支持的 provider: $1"
            ;;
    esac
}

provider_base_url_var() {
    case "$1" in
        openai)
            echo "OPENAI_BASE_URL"
            ;;
        deepseek)
            echo "DEEPSEEK_BASE_URL"
            ;;
        bigmodel)
            echo "BIGMODEL_BASE_URL"
            ;;
        auto)
            echo "OPENAI_BASE_URL"
            ;;
        mimo)
            echo ""
            ;;
        *)
            die "不支持的 provider: $1"
            ;;
    esac
}

provider_model_var() {
    case "$1" in
        deepseek)
            echo "DEEPSEEK_MODEL"
            ;;
        bigmodel)
            echo "BIGMODEL_MODEL"
            ;;
        openai|mimo|auto)
            echo ""
            ;;
        *)
            die "不支持的 provider: $1"
            ;;
    esac
}

config_show_cmd() {
    local keys=("$@")
    local key value shown

    if ((${#keys[@]} == 0)); then
        keys=(
            QQ_BOT_APP_ID
            QQ_BOT_APP_SECRET
            QQ_BOT_SANDBOX
            QQ_MAID_STATUS_DISPLAY_NAME
            QQ_MAID_GROUP_MESSAGE_MODE
            QQ_MAID_GROUP_ACTIVE_KEYWORDS
            LLM_PROVIDER
            LLM_MODEL
            PRIVATE_LLM_MODEL
            GROUP_LLM_MODEL
            OPENAI_API_KEY
            OPENAI_BASE_URL
            OPENAI_API_MODE
            DEEPSEEK_API_KEY
            DEEPSEEK_BASE_URL
            BIGMODEL_API_KEY
            BIGMODEL_BASE_URL
            MIMO_API_KEY
            OPENAI_SEARCH_MODEL
        )
    fi

    for key in "${keys[@]}"; do
        value="$(get_env_var "${key}")"
        shown="$(mask_config_value "${key}" "${value}")"
        printf '%s=%s\n' "${key}" "${shown}"
    done
}

config_set_cmd() {
    local pair key value

    (($# > 0)) || die "用法: qbot config set KEY=VALUE [KEY=VALUE]"
    for pair in "$@"; do
        [[ "${pair}" == *=* ]] || die "配置项必须是 KEY=VALUE: ${pair}"
        key="${pair%%=*}"
        value="${pair#*=}"
        set_env_var "${key}" "${value}"
        ui_out_status "${UI_GREEN}" "已设置:" "${key}"
    done
    config_done_hint
}

take_next_arg() {
    local option="$1"
    local value="${2:-}"
    [[ -n "${value}" ]] || die "${option} 缺少参数"
    echo "${value}"
}

PROMPT_KEEP="__QBOT_PROMPT_KEEP__"
PROMPT_CLEAR="__QBOT_PROMPT_CLEAR__"

prompt_read_secret_value() {
    local input="" char

    if [[ ! -t 0 ]]; then
        IFS= read -r input || return 1
        printf '\n' >&2
        echo "${input}"
        return 0
    fi

    while IFS= read -r -s -n 1 char; do
        case "${char}" in
            ""|$'\n'|$'\r')
                printf '\n' >&2
                echo "${input}"
                return 0
                ;;
            $'\177'|$'\b')
                if [[ -n "${input}" ]]; then
                    input="${input%?}"
                    printf '\b \b' >&2
                fi
                ;;
            $'\025')
                while [[ -n "${input}" ]]; do
                    input="${input%?}"
                    printf '\b \b' >&2
                done
                ;;
            *)
                input+="${char}"
                printf '*' >&2
                ;;
        esac
    done

    return 1
}

prompt_read_value() {
    local prompt="$1"
    local key="$2"
    local current="$3"
    local required="${4:-0}"
    local secret="${5:-0}"
    local input shown

    shown="$(prompt_display_default "${key}" "${current}")"
    while :; do
        printf '\n%b%s%b\n' "${UI_BOLD}" "${prompt}" "${UI_RESET}" >&2
        printf '  当前值: %b%s%b\n' "${UI_YELLOW}" "${shown}" "${UI_RESET}" >&2
        printf '  %b请输入新值后按回车；留空保留当前值' "${UI_DIM}" >&2
        if [[ "${required}" != "1" ]]; then
            printf '；输入 - 清空' >&2
        fi
        printf '%b\n' "${UI_RESET}" >&2
        printf '  %b>%b ' "${UI_CYAN}" "${UI_RESET}" >&2

        if [[ "${secret}" == "1" ]]; then
            input="$(prompt_read_secret_value)" || die "读取输入失败"
        else
            IFS= read -r input || die "读取输入失败"
            [[ -t 0 ]] || printf '\n' >&2
        fi

        if [[ -z "${input}" ]]; then
            if [[ "${required}" == "1" && -z "${current}" ]]; then
                ui_fail "此项为必填项，请输入后按回车。"
                continue
            fi
            echo "${PROMPT_KEEP}"
            return 0
        fi

        if [[ "${input}" == "-" ]]; then
            if [[ "${required}" == "1" ]]; then
                ui_fail "此项为必填项，不能清空。"
                continue
            fi
            echo "${PROMPT_CLEAR}"
            return 0
        fi

        echo "${input}"
        return 0
    done
}

prompt_choice_value() {
    local prompt="$1"
    local key="$2"
    local current="$3"
    local choices="$4"
    local required="${5:-1}"
    local input shown

    shown="$(prompt_display_default "${key}" "${current}")"
    while :; do
        printf '\n%b%s%b\n' "${UI_BOLD}" "${prompt}" "${UI_RESET}" >&2
        printf '  可选值: %b%s%b\n' "${UI_CYAN}" "${choices}" "${UI_RESET}" >&2
        printf '  当前值: %b%s%b\n' "${UI_YELLOW}" "${shown}" "${UI_RESET}" >&2
        printf '  %b请输入选项后按回车；留空保留当前值%b\n' "${UI_DIM}" "${UI_RESET}" >&2
        printf '  %b>%b ' "${UI_CYAN}" "${UI_RESET}" >&2
        IFS= read -r input || die "读取输入失败"
        [[ -t 0 ]] || printf '\n' >&2

        if [[ -z "${input}" ]]; then
            if [[ "${required}" == "1" && -z "${current}" ]]; then
                ui_fail "此项为必填项，请输入一个可选值。"
                continue
            fi
            echo "${PROMPT_KEEP}"
            return 0
        fi

        case " ${choices//|/ } " in
            *" ${input} "*)
                echo "${input}"
                return 0
                ;;
            *)
                ui_fail "无效选项: ${input}。请从 ${choices} 中选择。"
                ;;
        esac
    done
}

prompt_model_value() {
    local prompt="$1"
    local key="$2"
    local current="$3"
    local required="${4:-0}"
    local models="$5"
    local input shown count i model page_size filter_lc limit match_count display_count next_count input_lc show_all
    local query char seq message selected selected_touched marker
    local -a model_items=()
    local -a display_items=()

    if [[ -n "${models}" ]]; then
        mapfile -t model_items <<< "${models}"
    fi

    shown="$(prompt_display_default "${key}" "${current}")"
    count="${#model_items[@]}"
    page_size="${QBOT_MODEL_LIST_PAGE_SIZE:-20}"
    [[ "${page_size}" =~ ^[0-9]+$ && "${page_size}" -gt 0 ]] || page_size=20

    if ((count > 0)) && [[ -t 0 && -t 2 && "${QBOT_MODEL_LIVE_FILTER:-1}" != "0" ]]; then
        query=""
        message=""
        selected=0
        selected_touched=0

        while :; do
            display_items=()
            match_count=0
            if [[ -n "${query}" && ! "${query}" =~ ^[0-9]+$ ]]; then
                filter_lc="${query,,}"
            else
                filter_lc=""
            fi

            for model in "${model_items[@]}"; do
                if [[ -z "${filter_lc}" || "${model,,}" == *"${filter_lc}"* ]]; then
                    ((match_count += 1))
                    if ((${#display_items[@]} < page_size)); then
                        display_items+=("${model}")
                    fi
                fi
            done
            display_count="${#display_items[@]}"
            if ((selected >= display_count)); then
                selected=$((display_count - 1))
            fi
            if ((selected < 0)); then
                selected=0
            fi

            if [[ "${QBOT_NO_CLEAR:-0}" != "1" ]]; then
                printf '\033[H\033[2J' >&2
            else
                printf '\n' >&2
            fi
            printf '%b%s%b\n' "${UI_BOLD}" "${prompt}" "${UI_RESET}" >&2
            printf '  当前值: %b%s%b\n' "${UI_YELLOW}" "${shown}" "${UI_RESET}" >&2
            if [[ -z "${query}" ]]; then
                printf '  输入: %b(空)%b  匹配: %b%d%b/%d\n' "${UI_DIM}" "${UI_RESET}" "${UI_YELLOW}" "${match_count}" "${UI_RESET}" "${count}" >&2
            elif [[ "${query}" =~ ^[0-9]+$ ]]; then
                printf '  序号: %b%s%b  匹配: %b%d%b/%d\n' "${UI_CYAN}" "${query}" "${UI_RESET}" "${UI_YELLOW}" "${match_count}" "${UI_RESET}" "${count}" >&2
            else
                printf '  筛选: %b%s%b  匹配: %b%d%b/%d\n' "${UI_CYAN}" "${query}" "${UI_RESET}" "${UI_YELLOW}" "${match_count}" "${UI_RESET}" "${count}" >&2
            fi

            if ((display_count > 0)); then
                printf '  可用模型:\n' >&2
                for i in "${!display_items[@]}"; do
                    marker=" "
                    if ((i == selected)); then
                        marker=">"
                    fi
                    printf '    %b%s%b %d - %s\n' "${UI_GREEN}" "${marker}" "${UI_RESET}" "$((i + 1))" "${display_items[$i]}" >&2
                done
                if ((match_count > display_count)); then
                    printf '  %b已显示前 %d 个，共 %d 个匹配；继续输入可缩小范围。%b\n' "${UI_DIM}" "${display_count}" "${match_count}" "${UI_RESET}" >&2
                fi
            else
                printf '  %b没有匹配的模型。%b\n' "${UI_YELLOW}" "${UI_RESET}" >&2
            fi

            if [[ -n "${message}" ]]; then
                printf '  %b%s%b\n' "${UI_RED}" "${message}" "${UI_RESET}" >&2
            fi
            printf '  %b输入关键词实时筛选；↑/↓ 选择；Enter 确认；Backspace 删除；Ctrl+U 清空输入' "${UI_DIM}" >&2
            if [[ "${required}" != "1" ]]; then
                printf '；输入 - 后回车清空' >&2
            fi
            printf '；留空回车保留当前值%b\n' "${UI_RESET}" >&2
            printf '  %b>%b %s' "${UI_CYAN}" "${UI_RESET}" "${query}" >&2

            IFS= read -r -s -n 1 char || die "读取输入失败"
            message=""
            case "${char}" in
                ""|$'\n'|$'\r')
                    printf '\n' >&2
                    if [[ -z "${query}" ]]; then
                        if ((selected_touched == 1 && display_count > 0)); then
                            echo "${display_items[$selected]}"
                            return 0
                        fi
                        if [[ "${required}" == "1" && -z "${current}" ]]; then
                            message="此项为必填项，请输入关键词、序号或模型名。"
                            continue
                        fi
                        echo "${PROMPT_KEEP}"
                        return 0
                    fi

                    if [[ "${query}" == "-" ]]; then
                        if [[ "${required}" == "1" ]]; then
                            message="此项为必填项，不能清空。"
                            continue
                        fi
                        echo "${PROMPT_CLEAR}"
                        return 0
                    fi

                    if [[ "${query}" =~ ^[0-9]+$ ]]; then
                        if ((query >= 1 && query <= display_count)); then
                            echo "${display_items[$((query - 1))]}"
                            return 0
                        fi
                        message="无效序号: ${query}。请输入当前显示列表 1-${display_count} 之间的序号。"
                        continue
                    fi

                    for model in "${model_items[@]}"; do
                        if [[ "${query}" == "${model}" ]]; then
                            echo "${query}"
                            return 0
                        fi
                    done

                    if ((display_count > 0)); then
                        echo "${display_items[$selected]}"
                        return 0
                    fi

                    message="模型不在列表中，也没有匹配关键词: ${query}。"
                    ;;
                $'\177'|$'\b')
                    query="${query%?}"
                    selected=0
                    selected_touched=0
                    ;;
                $'\025')
                    query=""
                    selected=0
                    selected_touched=0
                    ;;
                $'\033')
                    seq=""
                    IFS= read -r -s -n 2 -t 0.02 seq || true
                    case "${seq}" in
                        "[A")
                            if ((display_count > 0)); then
                                if ((selected > 0)); then
                                    ((selected -= 1))
                                fi
                                selected_touched=1
                            fi
                            ;;
                        "[B")
                            if ((display_count > 0)); then
                                if ((selected < display_count - 1)); then
                                    ((selected += 1))
                                fi
                                selected_touched=1
                            fi
                            ;;
                    esac
                    ;;
                *)
                    query+="${char}"
                    selected=0
                    selected_touched=0
                    ;;
            esac
        done
    fi

    local filter=""
    show_all=0

    while :; do
        display_items=()
        match_count=0
        if ((count > 0)); then
            filter_lc="${filter,,}"
            if [[ "${show_all}" == "1" ]]; then
                limit="${count}"
            else
                limit="${page_size}"
            fi

            for model in "${model_items[@]}"; do
                if [[ -z "${filter_lc}" || "${model,,}" == *"${filter_lc}"* ]]; then
                    ((match_count += 1))
                    if ((${#display_items[@]} < limit)); then
                        display_items+=("${model}")
                    fi
                fi
            done
        fi
        display_count="${#display_items[@]}"

        printf '\n%b%s%b\n' "${UI_BOLD}" "${prompt}" "${UI_RESET}" >&2
        printf '  当前值: %b%s%b\n' "${UI_YELLOW}" "${shown}" "${UI_RESET}" >&2

        if ((count > 0)); then
            if [[ -n "${filter}" ]]; then
                printf '  筛选: %b%s%b  匹配: %b%d%b/%d\n' "${UI_CYAN}" "${filter}" "${UI_RESET}" "${UI_YELLOW}" "${match_count}" "${UI_RESET}" "${count}" >&2
            else
                printf '  模型总数: %b%d%b\n' "${UI_YELLOW}" "${count}" "${UI_RESET}" >&2
            fi

            if ((match_count > 0)); then
                printf '  可用模型:\n' >&2
                for i in "${!display_items[@]}"; do
                    printf '    %d - %s\n' "$((i + 1))" "${display_items[$i]}" >&2
                done
                if ((match_count > display_count)); then
                    printf '  %b已显示前 %d 个，共 %d 个匹配；输入更精确的关键词可缩小范围。%b\n' "${UI_DIM}" "${display_count}" "${match_count}" "${UI_RESET}" >&2
                fi
            else
                printf '  %b没有匹配的模型；输入 / 重置筛选，或换一个关键词。%b\n' "${UI_YELLOW}" "${UI_RESET}" >&2
            fi
            printf '  %b请输入当前列表序号或完整模型名；输入关键词筛选；输入 / 重置筛选；输入 /all 显示全部；留空保留当前值' "${UI_DIM}" >&2
        else
            printf '  %b未能获取模型列表，请输入模型名后按回车；留空保留当前值' "${UI_DIM}" >&2
        fi

        if [[ "${required}" != "1" ]]; then
            printf '；输入 - 清空' >&2
        fi
        printf '%b\n' "${UI_RESET}" >&2
        printf '  %b>%b ' "${UI_CYAN}" "${UI_RESET}" >&2
        IFS= read -r input || die "读取输入失败"
        [[ -t 0 ]] || printf '\n' >&2

        if [[ -z "${input}" ]]; then
            if [[ "${required}" == "1" && -z "${current}" ]]; then
                ui_fail "此项为必填项，请输入序号或模型名。"
                continue
            fi
            echo "${PROMPT_KEEP}"
            return 0
        fi

        if [[ "${input}" == "-" ]]; then
            if [[ "${required}" == "1" ]]; then
                ui_fail "此项为必填项，不能清空。"
                continue
            fi
            echo "${PROMPT_CLEAR}"
            return 0
        fi

        if ((count > 0)); then
            case "${input}" in
                /)
                    filter=""
                    show_all=0
                    ui_clear_screen
                    continue
                    ;;
                /all)
                    show_all=1
                    ui_clear_screen
                    continue
                    ;;
                /*)
                    filter="${input#/}"
                    show_all=0
                    ui_clear_screen
                    continue
                    ;;
            esac

            if [[ "${input}" =~ ^[0-9]+$ ]]; then
                if ((input >= 1 && input <= display_count)); then
                    echo "${display_items[$((input - 1))]}"
                    return 0
                fi
                if ((display_count > 0)); then
                    ui_fail "无效序号: ${input}。请输入当前显示列表 1-${display_count} 之间的序号。"
                else
                    ui_fail "当前没有可选模型，请先输入 / 重置筛选或换一个关键词。"
                fi
                continue
            fi

            for model in "${model_items[@]}"; do
                if [[ "${input}" == "${model}" ]]; then
                    echo "${input}"
                    return 0
                fi
            done

            next_count=0
            input_lc="${input,,}"
            for model in "${model_items[@]}"; do
                if [[ "${model,,}" == *"${input_lc}"* ]]; then
                    ((next_count += 1))
                fi
            done
            if ((next_count > 0)); then
                filter="${input}"
                show_all=0
                ui_clear_screen
                continue
            fi

            ui_fail "模型不在列表中，也没有匹配关键词: ${input}。请输入列表序号、完整模型名或关键词。"
            continue
        fi

        echo "${input}"
        return 0
    done
}

apply_prompted_env_var() {
    local key="$1"
    local value="$2"

    case "${value}" in
        "${PROMPT_KEEP}")
            ui_out_status "${UI_BLUE}" "已保留:" "${key}"
            ;;
        "${PROMPT_CLEAR}")
            set_env_var "${key}" ""
            ui_out_status "${UI_YELLOW}" "已清空:" "${key}"
            ;;
        *)
            set_env_var "${key}" "${value}"
            ui_out_status "${UI_GREEN}" "已设置:" "${key}"
            ;;
    esac
}

default_model_for_provider() {
    case "$1" in
        openai)
            echo "gpt-5.6-luna"
            ;;
        deepseek)
            echo "deepseek-chat"
            ;;
        bigmodel)
            echo "glm-5.2"
            ;;
        mimo)
            echo "mimo-v2.5-pro"
            ;;
        auto)
            echo "openai:gpt-5.6-luna"
            ;;
    esac
}

config_bot_interactive() {
    local app_id app_secret sandbox display_name group_mode active_keywords mention_ids
    local current_app_id current_app_secret current_sandbox current_display_name current_group_mode current_active_keywords current_mention_ids

    ui_clear_screen
    ui_header "qbot 配置向导 - QQ Bot"
    ui_note "输入内容后按回车；留空保留当前值；可选项输入 - 表示清空；密钥输入会显示为 *。"

    current_app_id="$(get_real_env_var QQ_BOT_APP_ID)"
    current_app_secret="$(get_real_env_var QQ_BOT_APP_SECRET)"
    current_sandbox="$(get_real_env_var QQ_BOT_SANDBOX)"
    current_display_name="$(get_real_env_var QQ_MAID_STATUS_DISPLAY_NAME)"
    current_group_mode="$(get_real_env_var QQ_MAID_GROUP_MESSAGE_MODE)"
    current_active_keywords="$(get_real_env_var QQ_MAID_GROUP_ACTIVE_KEYWORDS)"
    current_mention_ids="$(get_real_env_var QQ_MAID_BOT_MENTION_IDS)"

    [[ -z "${current_sandbox}" ]] && current_sandbox="false"
    [[ -z "${current_group_mode}" ]] && current_group_mode="mention"

    app_id="$(prompt_read_value "QQ Bot AppID 用于识别官方机器人。" QQ_BOT_APP_ID "${current_app_id}" 1 0)"
    app_secret="$(prompt_read_value "QQ Bot AppSecret 用于获取访问令牌。" QQ_BOT_APP_SECRET "${current_app_secret}" 1 1)"
    sandbox="$(prompt_choice_value "是否使用 QQ 沙箱环境。" QQ_BOT_SANDBOX "${current_sandbox}" "true|false" 1)"
    display_name="$(prompt_read_value "前台状态提示里的机器人称呼。" QQ_MAID_STATUS_DISPLAY_NAME "${current_display_name}" 0 0)"
    group_mode="$(prompt_choice_value "群消息处理模式：off=忽略，command=只处理命令，mention=命令/@/回复，active=额外处理关键词。" QQ_MAID_GROUP_MESSAGE_MODE "${current_group_mode}" "off|command|mention|active" 1)"

    if [[ "${group_mode}" == "active" || ("${group_mode}" == "${PROMPT_KEEP}" && "${current_group_mode}" == "active") ]]; then
        active_keywords="$(prompt_read_value "群聊 active 模式触发关键词，多个词用英文逗号分隔。" QQ_MAID_GROUP_ACTIVE_KEYWORDS "${current_active_keywords}" 0 0)"
    else
        active_keywords="${PROMPT_KEEP}"
    fi
    mention_ids="$(prompt_read_value "机器人 mention 兜底 ID，多个 ID 用英文逗号分隔；通常可留空。" QQ_MAID_BOT_MENTION_IDS "${current_mention_ids}" 0 0)"

    apply_prompted_env_var QQ_BOT_APP_ID "${app_id}"
    apply_prompted_env_var QQ_BOT_APP_SECRET "${app_secret}"
    apply_prompted_env_var QQ_BOT_SANDBOX "${sandbox}"
    apply_prompted_env_var QQ_MAID_STATUS_DISPLAY_NAME "${display_name}"
    apply_prompted_env_var QQ_MAID_GROUP_MESSAGE_MODE "${group_mode}"
    apply_prompted_env_var QQ_MAID_GROUP_ACTIVE_KEYWORDS "${active_keywords}"
    apply_prompted_env_var QQ_MAID_BOT_MENTION_IDS "${mention_ids}"

    config_done_hint
}

config_bot_cmd() {
    local app_id="" app_secret="" sandbox="" display_name="" group_mode="" active_keywords="" mention_ids=""

    if (($# == 0)); then
        config_bot_interactive
        return 0
    fi

    while (($# > 0)); do
        case "$1" in
            --app-id)
                app_id="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --app-id=*)
                app_id="${1#*=}"
                shift
                ;;
            --app-secret)
                app_secret="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --app-secret=*)
                app_secret="${1#*=}"
                shift
                ;;
            --sandbox)
                sandbox="$(normalize_bool_value "$(take_next_arg "$1" "${2:-}")")"
                shift 2
                ;;
            --sandbox=*)
                sandbox="$(normalize_bool_value "${1#*=}")"
                shift
                ;;
            --display-name|--name)
                display_name="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --display-name=*|--name=*)
                display_name="${1#*=}"
                shift
                ;;
            --group-mode)
                group_mode="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --group-mode=*)
                group_mode="${1#*=}"
                shift
                ;;
            --active-keywords)
                active_keywords="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --active-keywords=*)
                active_keywords="${1#*=}"
                shift
                ;;
            --mention-ids)
                mention_ids="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --mention-ids=*)
                mention_ids="${1#*=}"
                shift
                ;;
            -h|--help)
                config_usage
                return 0
                ;;
            *)
                die "未知参数: $1"
                ;;
        esac
    done

    if [[ -n "${group_mode}" ]]; then
        case "${group_mode}" in
            off|command|mention|active) ;;
            *) die "--group-mode 只能是 off/command/mention/active" ;;
        esac
    fi

    [[ -n "${app_id}" ]] && set_env_var QQ_BOT_APP_ID "${app_id}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_BOT_APP_ID"
    [[ -n "${app_secret}" ]] && set_env_var QQ_BOT_APP_SECRET "${app_secret}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_BOT_APP_SECRET"
    [[ -n "${sandbox}" ]] && set_env_var QQ_BOT_SANDBOX "${sandbox}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_BOT_SANDBOX"
    [[ -n "${display_name}" ]] && set_env_var QQ_MAID_STATUS_DISPLAY_NAME "${display_name}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_MAID_STATUS_DISPLAY_NAME"
    [[ -n "${group_mode}" ]] && set_env_var QQ_MAID_GROUP_MESSAGE_MODE "${group_mode}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_MAID_GROUP_MESSAGE_MODE"
    [[ -n "${active_keywords}" ]] && set_env_var QQ_MAID_GROUP_ACTIVE_KEYWORDS "${active_keywords}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_MAID_GROUP_ACTIVE_KEYWORDS"
    [[ -n "${mention_ids}" ]] && set_env_var QQ_MAID_BOT_MENTION_IDS "${mention_ids}" && ui_out_status "${UI_GREEN}" "已设置:" "QQ_MAID_BOT_MENTION_IDS"

    config_done_hint
}

config_ai_interactive() {
    local provider api_key base_url model private_model group_model search_model api_mode
    local current_provider current_model current_private_model current_group_model current_search_model current_api_mode
    local key_var base_url_var model_var current_api_key current_base_url model_default llm_provider normalized_model
    local effective_api_key effective_base_url model_list model_count

    ui_clear_screen
    ui_header "qbot 配置向导 - AI 渠道"
    ui_note "输入内容后按回车；留空保留当前值；可选项输入 - 表示清空；API Key 输入会显示为 *。"
    ui_note "配置 Key 和 Base URL 后只请求一次 /models；后续选择和筛选都使用本次缓存。"

    current_provider="$(get_real_env_var LLM_PROVIDER)"
    [[ -z "${current_provider}" ]] && current_provider="auto"
    current_model="$(get_real_env_var LLM_MODEL)"
    current_private_model="$(get_real_env_var PRIVATE_LLM_MODEL)"
    current_group_model="$(get_real_env_var GROUP_LLM_MODEL)"
    current_search_model="$(get_real_env_var OPENAI_SEARCH_MODEL)"
    current_api_mode="$(get_real_env_var OPENAI_API_MODE)"
    [[ -z "${current_api_mode}" ]] && current_api_mode="auto"

    provider="$(prompt_choice_value "请选择默认 AI 渠道。" LLM_PROVIDER "${current_provider}" "openai|deepseek|bigmodel|mimo|auto" 1)"
    [[ "${provider}" == "${PROMPT_KEEP}" ]] && provider="${current_provider}"

    key_var="$(provider_key_var "${provider}")"
    base_url_var="$(provider_base_url_var "${provider}")"
    model_var="$(provider_model_var "${provider}")"

    if [[ -n "${key_var}" ]]; then
        current_api_key="$(get_real_env_var "${key_var}")"
        api_key="$(prompt_read_value "${provider} API Key。" "${key_var}" "${current_api_key}" 1 1)"
    else
        api_key="${PROMPT_KEEP}"
    fi

    if [[ -n "${base_url_var}" ]]; then
        current_base_url="$(get_real_env_var "${base_url_var}")"
        if [[ "${provider}" == "auto" ]]; then
            base_url="$(prompt_read_value "OpenAI 兼容 Base URL；可输入 https://网关 或 https://网关/v1，脚本会自动规范化为版本结尾。" "${base_url_var}" "${current_base_url}" 0 0)"
        else
            base_url="$(prompt_read_value "${provider} Base URL；使用官方默认地址时可留空，脚本只会去掉末尾斜杠。" "${base_url_var}" "${current_base_url}" 0 0)"
        fi
    else
        base_url="${PROMPT_KEEP}"
    fi

    effective_api_key="${api_key}"
    [[ "${effective_api_key}" == "${PROMPT_KEEP}" ]] && effective_api_key="${current_api_key:-}"
    [[ "${effective_api_key}" == "${PROMPT_CLEAR}" ]] && effective_api_key=""

    effective_base_url="${base_url}"
    [[ "${effective_base_url}" == "${PROMPT_KEEP}" ]] && effective_base_url="${current_base_url:-}"
    [[ "${effective_base_url}" == "${PROMPT_CLEAR}" ]] && effective_base_url=""
    if [[ -n "${effective_base_url}" ]]; then
        effective_base_url="$(normalize_base_url_value "${provider}" "${effective_base_url}")"
    fi

    ui_note "正在获取模型列表..."
    model_list="$(fetch_provider_models "${provider}" "${effective_api_key}" "${effective_base_url}" || true)"
    if [[ -z "${model_list}" ]]; then
        ui_warn "未能从 /models 获取模型列表，将临时允许输入模型名继续。"
    else
        model_count="$(printf '%s\n' "${model_list}" | awk 'NF { n += 1 } END { print n + 0 }')"
        ui_note "已缓存 ${model_count} 个模型；实时筛选不会再次请求接口。"
    fi

    model_default="${current_model}"
    [[ -z "${model_default}" ]] && model_default="$(default_model_for_provider "${provider}")"
    model="$(prompt_model_value "请选择默认聊天模型。" LLM_MODEL "${model_default}" 1 "${model_list}")"
    if [[ "${model}" == "${PROMPT_KEEP}" && -z "${current_model}" ]]; then
        model="${model_default}"
    fi

    private_model="$(prompt_model_value "请选择私聊模型；不单独区分私聊时可留空。" PRIVATE_LLM_MODEL "${current_private_model}" 0 "${model_list}")"
    group_model="$(prompt_model_value "请选择群聊模型；不单独区分群聊时可留空。" GROUP_LLM_MODEL "${current_group_model}" 0 "${model_list}")"
    if [[ "${provider}" == "openai" || "${provider}" == "auto" ]]; then
        search_model="$(prompt_model_value "请选择 /查 使用的 OpenAI Web Search 模型；不用 /查 时可留空。" OPENAI_SEARCH_MODEL "${current_search_model}" 0 "${model_list}")"
    else
        search_model="$(prompt_read_value "/查 使用 OpenAI Web Search 兼容模型；当前渠道不是 OpenAI，未拉取列表。不用 /查 时可留空。" OPENAI_SEARCH_MODEL "${current_search_model}" 0 0)"
    fi

    if [[ "${provider}" == "openai" ]]; then
        api_mode="$(prompt_choice_value "OpenAI API 模式；普通 OpenAI 用 auto，兼容网关通常用 chat_only。" OPENAI_API_MODE "${current_api_mode}" "auto|chat_only" 1)"
    else
        api_mode="${PROMPT_KEEP}"
    fi

    llm_provider="${provider}"
    [[ "${provider}" == "mimo" ]] && llm_provider="auto"
    set_env_var LLM_PROVIDER "${llm_provider}"
    ui_out_status "${UI_GREEN}" "已设置:" "LLM_PROVIDER"

    if [[ -n "${key_var}" ]]; then
        apply_prompted_env_var "${key_var}" "${api_key}"
    fi
    if [[ -n "${base_url_var}" ]]; then
        if [[ "${base_url}" != "${PROMPT_KEEP}" && "${base_url}" != "${PROMPT_CLEAR}" ]]; then
            base_url="$(normalize_base_url_value "${provider}" "${base_url}")"
        fi
        apply_prompted_env_var "${base_url_var}" "${base_url}"
    fi

    if [[ "${model}" != "${PROMPT_KEEP}" && "${model}" != "${PROMPT_CLEAR}" ]]; then
        normalized_model="$(normalize_model_value "${provider}" "${model}")"
        set_env_var LLM_MODEL "${normalized_model}"
        ui_out_status "${UI_GREEN}" "已设置:" "LLM_MODEL"
        if [[ -n "${model_var}" ]]; then
            set_env_var "${model_var}" "${normalized_model}"
            ui_out_status "${UI_GREEN}" "已设置:" "${model_var}"
        fi
    else
        apply_prompted_env_var LLM_MODEL "${model}"
    fi

    if [[ "${private_model}" != "${PROMPT_KEEP}" && "${private_model}" != "${PROMPT_CLEAR}" ]]; then
        set_env_var PRIVATE_LLM_MODEL "$(normalize_model_value "${provider}" "${private_model}")"
        ui_out_status "${UI_GREEN}" "已设置:" "PRIVATE_LLM_MODEL"
    else
        apply_prompted_env_var PRIVATE_LLM_MODEL "${private_model}"
    fi

    if [[ "${group_model}" != "${PROMPT_KEEP}" && "${group_model}" != "${PROMPT_CLEAR}" ]]; then
        set_env_var GROUP_LLM_MODEL "$(normalize_model_value "${provider}" "${group_model}")"
        ui_out_status "${UI_GREEN}" "已设置:" "GROUP_LLM_MODEL"
    else
        apply_prompted_env_var GROUP_LLM_MODEL "${group_model}"
    fi

    apply_prompted_env_var OPENAI_SEARCH_MODEL "${search_model}"
    apply_prompted_env_var OPENAI_API_MODE "${api_mode}"

    config_done_hint
}

config_ai_cmd() {
    local provider="" api_key="" base_url="" model="" private_model="" group_model="" search_model="" api_mode=""
    local key_var base_url_var model_var normalized_model llm_provider

    if (($# == 0)); then
        config_ai_interactive
        return 0
    fi

    while (($# > 0)); do
        case "$1" in
            --provider)
                provider="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --provider=*)
                provider="${1#*=}"
                shift
                ;;
            --api-key)
                api_key="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --api-key=*)
                api_key="${1#*=}"
                shift
                ;;
            --base-url)
                base_url="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --base-url=*)
                base_url="${1#*=}"
                shift
                ;;
            --model)
                model="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --model=*)
                model="${1#*=}"
                shift
                ;;
            --private-model)
                private_model="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --private-model=*)
                private_model="${1#*=}"
                shift
                ;;
            --group-model)
                group_model="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --group-model=*)
                group_model="${1#*=}"
                shift
                ;;
            --search-model)
                search_model="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --search-model=*)
                search_model="${1#*=}"
                shift
                ;;
            --api-mode)
                api_mode="$(take_next_arg "$1" "${2:-}")"
                shift 2
                ;;
            --api-mode=*)
                api_mode="${1#*=}"
                shift
                ;;
            -h|--help)
                config_usage
                return 0
                ;;
            *)
                die "未知参数: $1"
                ;;
        esac
    done

    provider="${provider:-auto}"
    case "${provider}" in
        openai|deepseek|bigmodel|mimo|auto) ;;
        *) die "--provider 只能是 openai/deepseek/bigmodel/mimo/auto" ;;
    esac
    if [[ -n "${api_mode}" ]]; then
        case "${api_mode}" in
            auto|chat_only) ;;
            *) die "--api-mode 只能是 auto/chat_only" ;;
        esac
    fi

    key_var="$(provider_key_var "${provider}")"
    base_url_var="$(provider_base_url_var "${provider}")"
    model_var="$(provider_model_var "${provider}")"
    llm_provider="${provider}"
    [[ "${provider}" == "mimo" ]] && llm_provider="auto"

    set_env_var LLM_PROVIDER "${llm_provider}"
    ui_out_status "${UI_GREEN}" "已设置:" "LLM_PROVIDER"

    if [[ -n "${api_key}" ]]; then
        [[ -n "${key_var}" ]] || die "auto provider 不能直接设置 --api-key，请用 qbot config set OPENAI_API_KEY=..."
        set_env_var "${key_var}" "${api_key}"
        ui_out_status "${UI_GREEN}" "已设置:" "${key_var}"
    fi

    if [[ -n "${base_url}" ]]; then
        [[ -n "${base_url_var}" ]] || die "${provider} 不支持通过 --base-url 配置；请用 qbot config set 或改 agent.toml"
        base_url="$(normalize_base_url_value "${provider}" "${base_url}")"
        set_env_var "${base_url_var}" "${base_url}"
        ui_out_status "${UI_GREEN}" "已设置:" "${base_url_var}"
    fi

    if [[ -n "${model}" ]]; then
        normalized_model="$(normalize_model_value "${provider}" "${model}")"
        set_env_var LLM_MODEL "${normalized_model}"
        ui_out_status "${UI_GREEN}" "已设置:" "LLM_MODEL"
        if [[ -n "${model_var}" ]]; then
            set_env_var "${model_var}" "${normalized_model}"
            ui_out_status "${UI_GREEN}" "已设置:" "${model_var}"
        fi
    fi

    if [[ -n "${private_model}" ]]; then
        set_env_var PRIVATE_LLM_MODEL "$(normalize_model_value "${provider}" "${private_model}")"
        ui_out_status "${UI_GREEN}" "已设置:" "PRIVATE_LLM_MODEL"
    fi

    if [[ -n "${group_model}" ]]; then
        set_env_var GROUP_LLM_MODEL "$(normalize_model_value "${provider}" "${group_model}")"
        ui_out_status "${UI_GREEN}" "已设置:" "GROUP_LLM_MODEL"
    fi

    if [[ -n "${search_model}" ]]; then
        set_env_var OPENAI_SEARCH_MODEL "${search_model}"
        ui_out_status "${UI_GREEN}" "已设置:" "OPENAI_SEARCH_MODEL"
    fi

    if [[ -n "${api_mode}" ]]; then
        set_env_var OPENAI_API_MODE "${api_mode}"
        ui_out_status "${UI_GREEN}" "已设置:" "OPENAI_API_MODE"
    fi

    config_done_hint
}

config_cmd() {
    local sub="${1:-}"
    [[ -n "${sub}" ]] || { config_usage; return 0; }
    shift || true

    case "${sub}" in
        show|list)
            config_show_cmd "$@"
            ;;
        get)
            [[ $# -eq 1 ]] || die "用法: qbot config get KEY"
            get_env_var "$1"
            ;;
        path)
            ensure_config_env_file
            ;;
        set)
            config_set_cmd "$@"
            ;;
        bot|qq)
            config_bot_cmd "$@"
            ;;
        ai|llm)
            config_ai_cmd "$@"
            ;;
        -h|--help|help)
            config_usage
            ;;
        *)
            die "未知 config 子命令: ${sub}"
            ;;
    esac
}

download_release() {
    local version="$1"
    local target="$2"
    local tmp_dir="$3"
    local package="qq-maid-bot-${version}-${target}"
    local archive_format="tar.gz"
    [[ "${target}" == windows-* ]] && archive_format="zip"
    local archive="${package}.${archive_format}"
    local raw_url="${RELEASES_URL}/download/${version}/${archive}"

    echo "下载 Release: ${version} (${target})" >&2
    download_github_file "${raw_url}" "${tmp_dir}/${archive}" "${archive}"
    download_github_file "${raw_url}.sha256" "${tmp_dir}/${archive}.sha256" "${archive}.sha256"

    (
        cd "${tmp_dir}"
        sha256sum -c "${archive}.sha256" >&2
        if [[ "${archive_format}" == "zip" ]]; then
            unzip -q "${archive}"
        else
            tar -xzf "${archive}"
        fi
    ) || die "Release 包校验或解压失败"

    [[ -d "${tmp_dir}/${package}" ]] || die "Release 包解压后目录不存在: ${package}"
    echo "${tmp_dir}/${package}"
}

copy_file_if_exists() {
    local src="$1"
    local dst="$2"
    local mode="${3:-0644}"

    [[ -f "${src}" ]] || return 0
    install -m "${mode}" "${src}" "${dst}"
}

merge_config() {
    local src_config="$1"
    local dst_config="$2"
    local version="$3"

    [[ -d "${src_config}" ]] || return 0
    mkdir -p "${dst_config}"

    local path rel dst
    while IFS= read -r -d '' path; do
        rel="${path#${src_config}/}"
        dst="${dst_config}/${rel}"
        mkdir -p "$(dirname -- "${dst}")"

        case "${rel}" in
            agent.toml)
                if [[ -f "${dst}" ]]; then
                    if ! cmp -s "${path}" "${dst}"; then
                        install -m 0644 "${path}" "${dst_config}/agent.toml.release-${version}"
                    fi
                else
                    install -m 0644 "${path}" "${dst}"
                fi
                ;;
            .env.example|*.example|*.example.*)
                install -m 0644 "${path}" "${dst}"
                ;;
            *)
                if [[ ! -e "${dst}" ]]; then
                    cp -a "${path}" "${dst}"
                fi
                ;;
        esac
    done < <(find "${src_config}" -type f -print0)
}

copy_release_into_app() {
    local release_dir="$1"
    local version="$2"

    mkdir -p "$(dirname -- "${APP_DIR}")"

    if [[ ! -d "${APP_DIR}" || -z "$(ls -A "${APP_DIR}" 2>/dev/null || true)" ]]; then
        rm -rf "${APP_DIR}"
        mv "${release_dir}" "${APP_DIR}"
    else
        mkdir -p "${APP_DIR}"
        copy_file_if_exists "${release_dir}/qq-maid-bot" "${APP_DIR}/qq-maid-bot" 0755
        copy_file_if_exists "${release_dir}/qq-maid-bot.exe" "${APP_DIR}/qq-maid-bot.exe" 0755

        local executable
        for executable in \
            botctl.sh \
            botmon.sh \
            diagnose-network.sh \
            validate-runtime.sh \
            qq-maid-healthcheck.sh \
            qq-maid-systemd.sh
        do
            copy_file_if_exists "${release_dir}/${executable}" "${APP_DIR}/${executable}" 0755
        done

        local data_file
        for data_file in README.md VERSION .env.example windows-startup-example.bat; do
            copy_file_if_exists "${release_dir}/${data_file}" "${APP_DIR}/${data_file}" 0644
        done

        if [[ -d "${release_dir}/static" ]]; then
            rm -rf "${APP_DIR}/static"
            cp -a "${release_dir}/static" "${APP_DIR}/static"
        fi

        merge_config "${release_dir}/config" "${APP_DIR}/config" "${version}"
        mkdir -p "${APP_DIR}/data/storage" "${APP_DIR}/logs" "${APP_DIR}/run"
    fi

    mkdir -p "${APP_DIR}/config" "${APP_DIR}/data/storage" "${APP_DIR}/logs" "${APP_DIR}/run"

    if [[ ! -f "${APP_DIR}/config/.env" && -f "${APP_DIR}/config/.env.example" ]]; then
        cp -n "${APP_DIR}/config/.env.example" "${APP_DIR}/config/.env"
        echo "已创建配置模板: ${APP_DIR}/config/.env"
    elif [[ ! -f "${APP_DIR}/config/.env" && -f "${APP_DIR}/.env.example" ]]; then
        cp -n "${APP_DIR}/.env.example" "${APP_DIR}/config/.env"
        echo "已创建配置模板: ${APP_DIR}/config/.env"
    fi

    chmod +x "${APP_DIR}/qq-maid-bot" "${APP_DIR}/qq-maid-bot.exe" "${APP_DIR}/botctl.sh" 2>/dev/null || true
}

install_or_update() {
    local command_name="$1"
    local requested_version="${2:-latest}"

    install_deps

    local target version current tmp_dir release_dir was_running
    target="$(detect_target)"
    version="$(resolve_version "${requested_version}")"
    current="$(local_version 2>/dev/null || true)"

    if [[ "${command_name}" == "update" && -n "${current}" && "$(normalize_version "${current}")" == "${version}" ]]; then
        echo "当前已是目标版本: ${current}"
        return 0
    fi

    tmp_dir="$(mktemp -d)"
    TMP_DIR_TO_CLEAN="${tmp_dir}"

    release_dir="$(download_release "${version}" "${target}" "${tmp_dir}")"

    was_running=0
    if is_qbot_running; then
        was_running=1
        echo "检测到 qbot 正在运行，准备替换文件前停止进程"
        run_botctl stop
    fi

    copy_release_into_app "${release_dir}" "${version}"
    rm -rf "${tmp_dir}"
    TMP_DIR_TO_CLEAN=""

    echo "qbot ${command_name} 完成: ${version}"
    echo "目录: ${APP_DIR}"
    echo "配置: ${APP_DIR}/config/.env"

    if ((was_running)); then
        echo "恢复启动 qbot"
        run_botctl start
    fi
}

show_version() {
    local current latest
    current="$(local_version 2>/dev/null || echo "未安装")"
    latest="$(latest_version 2>/dev/null || echo "无法获取")"
    echo "本地版本: ${current}"
    echo "最新版本: ${latest}"
}

deploy_qbot() {
    local install_path="${QBOT_INSTALL_PATH:-/usr/local/bin/qbot}"
    local source_path="${SELF}"

    [[ -f "${source_path}" ]] || die "无法定位脚本自身: ${source_path}"

    if [[ -e "${install_path}" ]] && [[ "$(readlink -f "${source_path}")" == "$(readlink -f "${install_path}")" ]]; then
        echo "已部署到: ${install_path}"
        return 0
    fi

    install -m 755 "${source_path}" "${install_path}"
    echo "已部署到: ${install_path}"
    echo "可直接使用: qbot <command>"
}

# 允许 Shell 回归测试仅加载函数，不触发命令分发。
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi

init_ui

if [[ $# -eq 0 ]]; then
    usage
    exit 0
fi

case "${1:-}" in
    start)
        run_botctl start
        ;;
    stop)
        run_botctl stop
        ;;
    restart)
        run_botctl restart
        ;;
    status)
        run_botctl status
        ;;
    log|logs)
        run_botctl logs
        ;;
    health)
        run_botctl health
        ;;
    console)
        run_botctl console
        ;;
    install)
        install_or_update install "${2:-latest}"
        ;;
    update|upgrade)
        install_or_update update "${2:-latest}"
        ;;
    patch)
        install_or_update update "${2:-latest}"
        ;;
    version)
        show_version
        ;;
    config)
        shift
        config_cmd "$@"
        ;;
    deploy|self-install)
        deploy_qbot
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage
        exit 1
        ;;
esac

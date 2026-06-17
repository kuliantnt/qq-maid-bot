#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
# 诊断脚本和控制脚本共用运行目录语义，避免启动和排障读取不同配置。
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${REPO_DIR}/runtime}"

GATEWAY_ENV_FILES=(
    "${REPO_DIR}/.env"
    "${RUNTIME_DIR}/.env"
    "${RUNTIME_DIR}/config/.env"
    "${REPO_DIR}/qq-maid-gateway-rs/config/.env"
)

LLM_ENV_FILES=(
    "${REPO_DIR}/.env"
    "${RUNTIME_DIR}/.env"
    "${RUNTIME_DIR}/config/.env"
    "${REPO_DIR}/qq-maid-llm/config/.env"
)

PUBLIC_IP_URLS=(
    "https://api.ipify.org"
    "https://ifconfig.me"
    "https://myip.ipip.net"
)

PROXY_KEYS=(
    "HTTP_PROXY"
    "HTTPS_PROXY"
    "ALL_PROXY"
    "http_proxy"
    "https_proxy"
    "all_proxy"
)

env_file_value() {
    local file="$1"
    local key="$2"
    local line name value

    [[ -f "${file}" ]] || return 1

    while IFS= read -r line || [[ -n "${line}" ]]; do
        line="${line%$'\r'}"
        [[ "${line}" =~ ^[[:space:]]*$ ]] && continue
        [[ "${line}" =~ ^[[:space:]]*# ]] && continue

        if [[ "${line}" =~ ^[[:space:]]*export[[:space:]]+([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]]; then
            name="${BASH_REMATCH[1]}"
            value="${BASH_REMATCH[2]}"
        elif [[ "${line}" =~ ^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]]; then
            name="${BASH_REMATCH[1]}"
            value="${BASH_REMATCH[2]}"
        else
            continue
        fi

        [[ "${name}" == "${key}" ]] || continue
        value="${value%"${value##*[![:space:]]}"}"

        if [[ "${value}" == \"*\" && "${value}" == *\" ]]; then
            value="${value#\"}"
            value="${value%\"}"
        elif [[ "${value}" == \'*\' && "${value}" == *\' ]]; then
            value="${value#\'}"
            value="${value%\'}"
        fi

        printf '%s\n' "${value}"
        return 0
    done < "${file}"

    return 1
}

lookup_env() {
    local key="$1"
    shift

    if [[ -n "${!key:-}" ]]; then
        printf '%s\n' "${!key}"
        return 0
    fi

    local file value
    for file in "$@"; do
        if value="$(env_file_value "${file}" "${key}")"; then
            [[ -n "${value}" ]] || continue
            printf '%s\n' "${value}"
            return 0
        fi
    done

    return 1
}

lookup_env_default() {
    local key="$1"
    local default="$2"
    shift 2

    lookup_env "${key}" "$@" || printf '%s\n' "${default}"
}

mask_value() {
    local value="${1:-}"
    local len

    if [[ -z "${value}" ]]; then
        printf '<missing>'
        return
    fi

    len="${#value}"
    if (( len <= 8 )); then
        printf '***'
    else
        printf '%s***%s' "${value:0:4}" "${value: -4}"
    fi
}

set_status() {
    if [[ -n "${1:-}" ]]; then
        printf '<set>'
    else
        printf '<missing>'
    fi
}

mask_url() {
    local value="${1:-}"

    if [[ -z "${value}" ]]; then
        printf '<missing>'
        return
    fi

    if [[ "${value}" =~ ^([^:]+://)([^/@]+)@(.+)$ ]]; then
        printf '%s***@%s' "${BASH_REMATCH[1]}" "${BASH_REMATCH[3]}"
    else
        printf '%s' "${value}"
    fi
}

fetch_url() {
    local url="$1"

    if command -v curl >/dev/null 2>&1; then
        curl -fsS --max-time 8 "${url}"
        return
    fi

    if command -v wget >/dev/null 2>&1; then
        wget -qO- --timeout=8 --tries=1 "${url}"
        return
    fi

    printf 'no curl or wget available'
    return 127
}

print_env_files() {
    local title="$1"
    shift

    printf '%s\n' "${title}"
    local file
    local -A seen=()
    for file in "$@"; do
        [[ -z "${seen[${file}]:-}" ]] || continue
        seen["${file}"]=1
        if [[ -f "${file}" ]]; then
            printf '  %s: present\n' "${file#"${REPO_DIR}/"}"
        else
            printf '  %s: missing\n' "${file#"${REPO_DIR}/"}"
        fi
    done
    printf '\n'
}

gateway_app_id="$(lookup_env QQ_BOT_APP_ID "${GATEWAY_ENV_FILES[@]}" || lookup_env QQ_APPID "${GATEWAY_ENV_FILES[@]}" || true)"
gateway_secret="$(lookup_env QQ_BOT_APP_SECRET "${GATEWAY_ENV_FILES[@]}" || lookup_env QQ_SECRET "${GATEWAY_ENV_FILES[@]}" || true)"
respond_url="$(lookup_env_default QQ_MAID_RESPOND_URL "http://127.0.0.1:8787/v1/respond" "${GATEWAY_ENV_FILES[@]}")"

llm_provider="$(lookup_env_default LLM_PROVIDER "openai" "${LLM_ENV_FILES[@]}")"
llm_model="$(lookup_env_default LLM_MODEL "gpt-5.5" "${LLM_ENV_FILES[@]}")"
openai_key="$(lookup_env OPENAI_API_KEY "${LLM_ENV_FILES[@]}" || true)"
deepseek_key="$(lookup_env DEEPSEEK_API_KEY "${LLM_ENV_FILES[@]}" || true)"
llm_host="$(lookup_env_default LLM_SERVER_HOST "127.0.0.1" "${LLM_ENV_FILES[@]}")"
llm_port="$(lookup_env_default LLM_SERVER_PORT "8787" "${LLM_ENV_FILES[@]}")"
llm_url="$(lookup_env LLM_SERVER_URL "${LLM_ENV_FILES[@]}" || printf 'http://%s:%s\n' "${llm_host}" "${llm_port}")"

printf 'QQ Maid network diagnostics\n'
printf 'Repository: %s\n\n' "${REPO_DIR}"

print_env_files "Env files:" "${GATEWAY_ENV_FILES[@]}" "${LLM_ENV_FILES[@]}"

printf 'Gateway config:\n'
printf '  QQ_BOT_APP_ID: %s\n' "$(mask_value "${gateway_app_id}")"
printf '  QQ_BOT_APP_SECRET: %s\n' "$(set_status "${gateway_secret}")"
printf '  QQ_MAID_RESPOND_URL: %s\n\n' "$(mask_url "${respond_url}")"

printf 'LLM config:\n'
printf '  LLM_PROVIDER: %s\n' "${llm_provider}"
printf '  LLM_MODEL: %s\n' "${llm_model}"
printf '  LLM_SERVER_URL: %s\n' "$(mask_url "${llm_url}")"
printf '  OPENAI_API_KEY: %s\n' "$(set_status "${openai_key}")"
printf '  DEEPSEEK_API_KEY: %s\n\n' "$(set_status "${deepseek_key}")"

printf 'Proxy env:\n'
for key in "${PROXY_KEYS[@]}"; do
    printf '  %s=%s\n' "${key}" "$(mask_url "${!key:-}")"
done
printf '\n'

printf 'LLM health:\n'
health_url="${llm_url%/}/healthz"
if output="$(fetch_url "${health_url}" 2>&1)"; then
    printf '  %s -> %s\n' "${health_url}" "${output}"
else
    printf '  %s -> ERROR: %s\n' "${health_url}" "${output}"
fi
printf '\n'

printf 'Public IP checks:\n'
for url in "${PUBLIC_IP_URLS[@]}"; do
    if output="$(fetch_url "${url}" 2>&1)"; then
        printf '  %s -> %s\n' "${url}" "${output}"
    else
        printf '  %s -> ERROR: %s\n' "${url}" "${output}"
    fi
done

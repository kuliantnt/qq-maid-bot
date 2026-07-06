#!/usr/bin/env bash
set -Eeuo pipefail

# ============================================================
# botmon.sh - QQ Maid 进程监控采样与告警脚本
#
# 用于查看进程状态、资源占用、定时采样、阈值告警和 cron 集成。
# 与 botctl.sh 共享 runtime 目录和 .env 解析逻辑。
# ============================================================

# ---- 脚本目录与运行时目录解析 ----
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename -- "${BASH_SOURCE[0]}")"

# 兼容开发环境：当脚本在 scripts/ 运行时，runtime 在 ../runtime；
# 当脚本已在 runtime/ 目录时（含 config/ 子目录），直接使用自身所在目录。
if [[ "${SCRIPT_NAME}" == "botmon.sh" && -d "${SCRIPT_DIR}/config" ]]; then
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    DEFAULT_RUNTIME_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/../runtime" && pwd)"
fi
# 优先级: QQ_MAID_RUNTIME_DIR > BOT_RUNTIME_DIR > DEFAULT_RUNTIME_DIR
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${BOT_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}}"

# ---- env 文件解析 ----
resolve_env_file() {
    if [[ -n "${BOT_ENV_FILE:-}" ]]; then
        echo "${BOT_ENV_FILE}"
        return 0
    fi

    local candidate
    for candidate in \
        "${RUNTIME_DIR}/config/.env" \
        "${RUNTIME_DIR}/.env"
    do
        if [[ -f "${candidate}" ]]; then
            echo "${candidate}"
            return 0
        fi
    done

    return 1
}

load_env() {
    local env_file
    if ! env_file="$(resolve_env_file)"; then
        return 0
    fi
    [[ -f "${env_file}" ]] || return 0

    set -a
    set +u
    # shellcheck source=/dev/null
    . "${env_file}"
    set -u
    set +a
}

# ---- 加载运行时 env ----
# 在初始化 PID_FILE、HEALTH_URL 等路径前加载 .env，
# 以便 LLM_SERVER_HOST/PORT 和 BOT_* 覆盖项生效。
RESOLVED_ENV_FILE="$(resolve_env_file 2>/dev/null || true)"
load_env

# ---- 服务地址推导 ----
server_url() {
    local host port
    host="${LLM_SERVER_HOST:-127.0.0.1}"
    port="${LLM_SERVER_PORT:-8787}"
    echo "${LLM_SERVER_URL:-http://${host}:${port}}"
}

_default_health_url() {
    echo "$(server_url | sed 's:/*$::')/healthz"
}

# ---- 路径与阈值变量 ----
PID_FILE="${BOT_PID_FILE:-$RUNTIME_DIR/run/qq-maid-bot.pid}"
HEALTH_URL="${BOT_HEALTH_URL:-$(_default_health_url)}"

LOG_DIR="${BOT_LOG_DIR:-$RUNTIME_DIR/logs}"
METRIC_LOG="${BOT_METRIC_LOG:-$LOG_DIR/botmon-monitor.tsv}"
ALERT_LOG="${BOT_ALERT_LOG:-$LOG_DIR/botmon-alert.log}"
LOCK_FILE="$LOG_DIR/botmon-monitor.lock"

RSS_WARN_KB="${BOT_RSS_WARN_KB:-80000}"
RSS_CRIT_KB="${BOT_RSS_CRIT_KB:-150000}"
PRIVATE_DIRTY_WARN_KB="${BOT_PRIVATE_DIRTY_WARN_KB:-80000}"
FD_WARN="${BOT_FD_WARN:-200}"

mkdir -p "$LOG_DIR"

usage() {
  cat <<USAGE
botmon - QQ Maid monitor & metrics

Usage:
  botmon status              Show current bot status
  botmon sample              Append one monitor sample
  botmon watch [seconds]     Watch status repeatedly, default 5s
  botmon log [lines]         Show monitor log, default 20 lines
  botmon alerts [lines]      Show alert log, default 50 lines
  botmon cron-install        Install cron monitor, every 5 minutes
  botmon cron-remove         Remove cron monitor
  botmon paths               Show paths and config

Env override:
  QQ_MAID_RUNTIME_DIR        Runtime directory
  BOT_RUNTIME_DIR            (fallback) Runtime directory
  BOT_ENV_FILE               Env file to load
  BOT_PID_FILE               PID file path
  BOT_HEALTH_URL             Full healthz URL (overrides LLM_SERVER_*)
  BOT_LOG_DIR                Log directory
  BOT_METRIC_LOG             Metric TSV log path
  BOT_ALERT_LOG              Alert log path
  BOT_RSS_WARN_KB            RSS warning threshold
  BOT_RSS_CRIT_KB            RSS critical threshold
  BOT_PRIVATE_DIRTY_WARN_KB  Private dirty warning threshold
  BOT_FD_WARN                FD count warning threshold
USAGE
}

kb_to_mb() {
  awk -v kb="${1:-0}" 'BEGIN { printf "%.1f MB", kb / 1024 }'
}

# format_uptime 把秒数格式化为 "X年Y月Z天A小时B分C秒"，
# 其中年按365天、月按30天折算；为零的单位不显示。
format_uptime() {
  local s="${1:-0}"
  local y m d h min rest
  local parts=()

  y=$(( s / 31536000 ))
  rest=$(( s % 31536000 ))
  m=$(( rest / 2592000 ))
  rest=$(( rest % 2592000 ))
  d=$(( rest / 86400 ))
  rest=$(( rest % 86400 ))
  h=$(( rest / 3600 ))
  rest=$(( rest % 3600 ))
  min=$(( rest / 60 ))
  rest=$(( rest % 60 ))

  (( y  > 0 )) && parts+=("${y}年")
  (( m  > 0 )) && parts+=("${m}月")
  (( d  > 0 )) && parts+=("${d}天")
  (( h  > 0 )) && parts+=("${h}小时")
  (( min > 0 )) && parts+=("${min}分")
  parts+=("${rest}秒")  # 剩余秒数始终显示

  (
    IFS=''
    printf '%s' "${parts[*]}"
  )
}

get_pid() {
  if [[ ! -f "$PID_FILE" ]]; then
    echo ""
    return
  fi
  tr -d '[:space:]' < "$PID_FILE" || true
}

status_kb() {
  local pid="$1"
  local key="$2"
  awk -v k="$key:" '$1 == k { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/status" 2>/dev/null
}

status_value() {
  local pid="$1"
  local key="$2"
  awk -v k="$key:" '$1 == k { $1=""; sub(/^ /, ""); print; found=1 } END { if (!found) print "-" }' "/proc/$pid/status" 2>/dev/null
}

smaps_kb() {
  local pid="$1"
  local key="$2"
  awk -v k="$key:" '$1 == k { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/smaps_rollup" 2>/dev/null
}

health_code() {
  curl -fsS -o /dev/null -w '%{http_code}' --max-time 3 "$HEALTH_URL" 2>/dev/null || echo 000
}

ensure_header() {
  if [[ ! -s "$METRIC_LOG" ]]; then
    printf 'time\thost\tpid\thealth\tstate\tetimes_sec\tcpu_pct\tmem_pct\trss_kb\tpss_kb\tprivate_dirty_kb\tswap_kb\tfd_count\tthreads\tvm_size_kb\tvm_rss_kb\n' >> "$METRIC_LOG"
  fi
}

alert() {
  local level="$1"
  local msg="$2"
  printf '%s\t%s\t%s\t%s\n' "$(date --iso-8601=seconds)" "$(hostname)" "$level" "$msg" >> "$ALERT_LOG"
}

# collect 采集当前进程指标，存入全局变量。
# 返回码：
#   0  - 采集成功
#   10 - pid 文件缺失或内容不是合法数字
#   11 - pid 文件存在且合法，但对应进程不存在（/proc/$pid 不存在）
collect() {
  pid="$(get_pid)"

  if [[ -z "$pid" || ! "$pid" =~ ^[0-9]+$ ]]; then
    return 10
  fi

  if [[ ! -d "/proc/$pid" ]]; then
    return 11
  fi

  health="$(health_code)"
  state="$(status_value "$pid" State)"
  threads="$(status_value "$pid" Threads)"
  vm_size_kb="$(status_kb "$pid" VmSize)"
  vm_rss_kb="$(status_kb "$pid" VmRSS)"
  rss_kb="$(smaps_kb "$pid" Rss)"
  pss_kb="$(smaps_kb "$pid" Pss)"
  private_dirty_kb="$(smaps_kb "$pid" Private_Dirty)"
  swap_kb="$(smaps_kb "$pid" Swap)"
  fd_count="$(find "/proc/$pid/fd" -maxdepth 1 -type l 2>/dev/null | wc -l | tr -d ' ')"
  etimes_sec="$(ps -p "$pid" -o etimes= 2>/dev/null | awk '{print $1+0}')"
  cpu_pct="$(ps -p "$pid" -o %cpu= 2>/dev/null | awk '{print $1+0}')"
  mem_pct="$(ps -p "$pid" -o %mem= 2>/dev/null | awk '{print $1+0}')"

  return 0
}

# print_status 打印进程状态信息并返回码：
#   0 - 采集成功且健康状态正常
#   1 - 采集成功但存在告警 / collect 失败
# 不做 exit，由调用方决定是否退出。
print_status() {
  set +e
  collect
  local rc=$?
  set -e
  if (( rc != 0 )); then
    case "$rc" in
      10)
        echo "QQ Maid: pid file missing or invalid"
        echo "PID file: $PID_FILE"
        ;;
      11)
        echo "QQ Maid: not running"
        echo "PID file: $PID_FILE"
        ;;
      *)
        echo "QQ Maid: unknown status error (rc=$rc)"
        ;;
    esac
    return 1
  fi

  echo "QQ Maid status"
  echo "────────────────────────────────────────"
  printf "%-18s %s\n" "PID" "$pid"
  printf "%-18s %s\n" "Health" "$health"
  printf "%-18s %s\n" "State" "$state"
  printf "%-18s %s\n" "Uptime" "$(format_uptime "$etimes_sec")"
  printf "%-18s %s\n" "CPU" "${cpu_pct}%"
  printf "%-18s %s\n" "Mem" "${mem_pct}%"
  printf "%-18s %s / %s\n" "RSS" "$rss_kb KB" "$(kb_to_mb "$rss_kb")"
  printf "%-18s %s / %s\n" "PSS" "$pss_kb KB" "$(kb_to_mb "$pss_kb")"
  printf "%-18s %s / %s\n" "Private Dirty" "$private_dirty_kb KB" "$(kb_to_mb "$private_dirty_kb")"
  printf "%-18s %s / %s\n" "Swap" "$swap_kb KB" "$(kb_to_mb "$swap_kb")"
  printf "%-18s %s\n" "FD" "$fd_count"
  printf "%-18s %s\n" "Threads" "$threads"
  printf "%-18s %s / %s\n" "VmSize" "$vm_size_kb KB" "$(kb_to_mb "$vm_size_kb")"
  printf "%-18s %s\n" "Runtime" "$RUNTIME_DIR"
  echo

  if [[ "$health" != "200" ]]; then
    echo "⚠ healthz is not 200"
    return 1
  elif (( rss_kb >= RSS_CRIT_KB )); then
    echo "🚨 RSS is too high"
    return 1
  elif (( rss_kb >= RSS_WARN_KB )); then
    echo "⚠ RSS is high"
    return 1
  elif (( private_dirty_kb >= PRIVATE_DIRTY_WARN_KB )); then
    echo "⚠ Private_Dirty is high"
    return 1
  elif (( swap_kb > 0 )); then
    echo "⚠ Swap is used"
    return 1
  elif (( fd_count >= FD_WARN )); then
    echo "⚠ FD count is high"
    return 1
  else
    echo "✅ Looks healthy"
    return 0
  fi
}

cmd_status() {
  print_status
}

cmd_sample() {
  exec 9>"$LOCK_FILE"
  flock -n 9 || exit 0

  ensure_header

  set +e
  collect
  local rc=$?
  set -e
  if (( rc != 0 )); then
    case "$rc" in
      10) alert "CRIT" "pid_file_missing_or_invalid path=$PID_FILE" ;;
      11) alert "CRIT" "process_not_running pid=$(get_pid)" ;;
      *)  alert "CRIT" "unknown_collect_error rc=$rc" ;;
    esac
    exit 0
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(date --iso-8601=seconds)" "$(hostname)" "$pid" "$health" "$state" \
    "$etimes_sec" "$cpu_pct" "$mem_pct" "$rss_kb" "$pss_kb" "$private_dirty_kb" \
    "$swap_kb" "$fd_count" "$threads" "$vm_size_kb" "$vm_rss_kb" \
    >> "$METRIC_LOG"

  [[ "$health" == "200" ]] || alert "CRIT" "health_not_200 pid=$pid health=$health url=$HEALTH_URL"

  if (( rss_kb >= RSS_CRIT_KB )); then
    alert "CRIT" "rss_too_high pid=$pid rss_kb=$rss_kb threshold=$RSS_CRIT_KB"
  elif (( rss_kb >= RSS_WARN_KB )); then
    alert "WARN" "rss_high pid=$pid rss_kb=$rss_kb threshold=$RSS_WARN_KB"
  fi

  (( private_dirty_kb < PRIVATE_DIRTY_WARN_KB )) || alert "WARN" "private_dirty_high pid=$pid private_dirty_kb=$private_dirty_kb threshold=$PRIVATE_DIRTY_WARN_KB"
  (( swap_kb == 0 )) || alert "WARN" "swap_used pid=$pid swap_kb=$swap_kb"
  (( fd_count < FD_WARN )) || alert "WARN" "fd_count_high pid=$pid fd_count=$fd_count threshold=$FD_WARN"
}

cmd_watch() {
  local interval="${1:-5}"
  while true; do
    clear
    date
    echo
    cmd_status || true
    sleep "$interval"
  done
}

cmd_log() {
  local lines="${1:-20}"
  if [[ -f "$METRIC_LOG" ]]; then
    tail -n "$lines" "$METRIC_LOG"
  else
    echo "No monitor log yet: $METRIC_LOG"
  fi
}

cmd_alerts() {
  local lines="${1:-50}"
  if [[ -f "$ALERT_LOG" ]]; then
    tail -n "$lines" "$ALERT_LOG"
  else
    echo "No alert log yet: $ALERT_LOG"
  fi
}

cmd_cron_install() {
  local botmon_path="${SCRIPT_DIR}/${SCRIPT_NAME}"
  local entry

  # cron 环境下也需要加载 .env，因此显式传入 BOT_ENV_FILE（如果已解析到）
  if [[ -n "${RESOLVED_ENV_FILE:-}" && -f "${RESOLVED_ENV_FILE}" ]]; then
    entry="*/5 * * * * BOT_ENV_FILE=${RESOLVED_ENV_FILE} ${botmon_path} sample # botmon-monitor"
  else
    entry="*/5 * * * * ${botmon_path} sample # botmon-monitor"
  fi

  {
    crontab -l 2>/dev/null | grep -v 'botmon-monitor' || true
    echo "$entry"
  } | crontab -
  echo "Installed cron:"
  echo "$entry"
}

cmd_cron_remove() {
  crontab -l 2>/dev/null | grep -v 'botmon-monitor' | crontab - || true
  echo "Removed botmon monitor cron."
}

cmd_paths() {
  cat <<PATHS
SCRIPT_DIR=$SCRIPT_DIR
RUNTIME_DIR=$RUNTIME_DIR
ENV_FILE=${RESOLVED_ENV_FILE:-(none)}
PID_FILE=$PID_FILE
HEALTH_URL=$HEALTH_URL
LOG_DIR=$LOG_DIR
METRIC_LOG=$METRIC_LOG
ALERT_LOG=$ALERT_LOG

Thresholds:
RSS_WARN_KB=$RSS_WARN_KB
RSS_CRIT_KB=$RSS_CRIT_KB
PRIVATE_DIRTY_WARN_KB=$PRIVATE_DIRTY_WARN_KB
FD_WARN=$FD_WARN
PATHS
}

# ---- 命令入口 ----
# status 在 bot 未运行时应返回非零退出码；
# watch 中通过 || true 抑制 set -e，避免循环中断。
case "${1:-status}" in
  status) cmd_status || exit $? ;;
  sample) cmd_sample ;;
  watch) shift; cmd_watch "${1:-5}" ;;
  log) shift; cmd_log "${1:-20}" ;;
  alerts) shift; cmd_alerts "${1:-50}" ;;
  cron-install) cmd_cron_install ;;
  cron-remove) cmd_cron_remove ;;
  paths) cmd_paths ;;
  help|-h|--help) usage ;;
  *)
    echo "Unknown command: $1"
    echo
    usage
    exit 2
    ;;
esac

#!/usr/bin/env bash
set -Eeuo pipefail

RUNTIME_DIR="${BOT_RUNTIME_DIR:-/root/project/qqbot/runtime}"
PID_FILE="${BOT_PID_FILE:-$RUNTIME_DIR/run/qq-maid-bot.pid}"
HEALTH_URL="${BOT_HEALTH_URL:-http://127.0.0.1:8787/healthz}"

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
  BOT_RUNTIME_DIR=/root/project/qqbot/runtime
  BOT_PID_FILE=/root/project/qqbot/runtime/run/qq-maid-bot.pid
  BOT_HEALTH_URL=http://127.0.0.1:8787/healthz
USAGE
}

kb_to_mb() {
  awk -v kb="${1:-0}" 'BEGIN { printf "%.1f MB", kb / 1024 }'
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

cmd_status() {
  if ! collect; then
    case "$?" in
      10)
        echo "QQ Maid: pid file missing or invalid"
        echo "PID file: $PID_FILE"
        ;;
      11)
        echo "QQ Maid: not running"
        echo "PID file: $PID_FILE"
        ;;
      *)
        echo "QQ Maid: unknown status error"
        ;;
    esac
    exit 1
  fi

  echo "QQ Maid status"
  echo "────────────────────────────────────────"
  printf "%-18s %s\n" "PID" "$pid"
  printf "%-18s %s\n" "Health" "$health"
  printf "%-18s %s\n" "State" "$state"
  printf "%-18s %s\n" "Uptime" "${etimes_sec}s"
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
  elif (( rss_kb >= RSS_CRIT_KB )); then
    echo "🚨 RSS is too high"
  elif (( rss_kb >= RSS_WARN_KB )); then
    echo "⚠ RSS is high"
  elif (( private_dirty_kb >= PRIVATE_DIRTY_WARN_KB )); then
    echo "⚠ Private_Dirty is high"
  elif (( swap_kb > 0 )); then
    echo "⚠ Swap is used"
  elif (( fd_count >= FD_WARN )); then
    echo "⚠ FD count is high"
  else
    echo "✅ Looks healthy"
  fi
}

cmd_sample() {
  exec 9>"$LOCK_FILE"
  flock -n 9 || exit 0

  ensure_header

  if ! collect; then
    case "$?" in
      10) alert "CRIT" "pid_file_missing_or_invalid path=$PID_FILE" ;;
      11) alert "CRIT" "process_not_running pid=$(get_pid)" ;;
      *)  alert "CRIT" "unknown_collect_error" ;;
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
  local entry="*/5 * * * * /usr/local/bin/botmon sample # botmon-monitor"
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
RUNTIME_DIR=$RUNTIME_DIR
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

case "${1:-status}" in
  status) cmd_status ;;
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

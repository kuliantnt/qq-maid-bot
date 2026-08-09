# qbot.sh 在 Release 解压后加载本文件；它不是独立的用户命令。

migrate_agent_web_search_config() {
    local file="${APP_DIR}/config/agent.toml"
    local tmp backup owner group mode has_tools

    [[ -f "${file}" ]] || return 0
    if [[ -L "${file}" ]]; then
        ui_warn "跳过符号链接 Agent 配置的联网搜索迁移: ${file}"
        return 0
    fi
    grep -Eq '^[[:space:]]*\[search_routes\.[A-Za-z0-9_-]+\][[:space:]]*(#.*)?$' "${file}" || return 0
    if grep -Eq '^[[:space:]]*\[tools\.web_search\.routes\.[A-Za-z0-9_-]+\][[:space:]]*(#.*)?$' "${file}"; then
        die "Agent 配置同时包含旧、新联网搜索 route，无法自动合并: ${file}"
    fi
    has_tools=0
    if grep -Eq '^[[:space:]]*\[tools\.web_search\][[:space:]]*(#.*)?$' "${file}"; then
        has_tools=1
    fi

    backup="$(next_agent_config_backup "${file}")"
    cp -a -- "${file}" "${backup}"
    tmp="$(mktemp "$(dirname -- "${file}")/.agent.toml.web-search.XXXXXX")"
    owner="$(stat -c '%u' "${file}" 2>/dev/null || true)"
    group="$(stat -c '%g' "${file}" 2>/dev/null || true)"
    mode="$(stat -c '%a' "${file}" 2>/dev/null || true)"
    if ! awk -v has_tools="${has_tools}" '
        BEGIN { inserted = 0 }
        /^[[:space:]]*\[search_routes\.[A-Za-z0-9_-]+\][[:space:]]*(#.*)?$/ {
            if (!has_tools && !inserted) {
                print "[tools.web_search]"
                print "backend = \"provider_native\""
                print "max_results = 5"
                print "search_depth = \"basic\""
                print "topic = \"general\""
                print "connect_timeout_seconds = 10"
                print "first_response_timeout_seconds = 30"
                print "total_timeout_seconds = 60"
                print ""
                inserted = 1
            }
            sub(/\[search_routes\./, "[tools.web_search.routes.")
        }
        { print }
    ' "${file}" > "${tmp}"; then
        rm -f -- "${tmp}"
        die "联网搜索配置迁移失败，原文件保留: ${file}"
    fi
    [[ -n "${owner}" && -n "${group}" ]] && chown "${owner}:${group}" "${tmp}" 2>/dev/null || true
    [[ -n "${mode}" ]] && chmod "${mode}" "${tmp}" 2>/dev/null || true
    if ! mv -- "${tmp}" "${file}"; then
        rm -f -- "${tmp}"
        die "联网搜索配置迁移失败，原文件保留，备份位于: ${backup}"
    fi
    echo "已将旧联网搜索 route 迁移到 tools.web_search，旧配置备份: ${backup}"
}

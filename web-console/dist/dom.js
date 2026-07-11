export function requiredElement(id, type) {
    const element = document.getElementById(id);
    if (!(element instanceof type)) {
        throw new Error(`页面缺少必要元素：${id}`);
    }
    return element;
}
export function setText(id, value) {
    const element = document.getElementById(id);
    if (element)
        element.textContent = value;
}
export function cell(value, className) {
    const element = document.createElement("td");
    element.textContent = value;
    if (className)
        element.className = className;
    return element;
}
export function stateLabel(value) {
    const labels = {
        online: "在线",
        offline: "离线",
        supported: "支持",
        disabled: "未启用",
        unsupported: "不支持",
        unknown: "未知",
        not_available: "不可用",
        not_configured: "未配置",
        available: "可用",
        error: "异常",
        unverified: "未验证",
    };
    return labels[value] ?? `未知（${value}）`;
}
export function formatMarker(value) {
    if (!value)
        return "不可用";
    if (value.startsWith("unix:")) {
        const seconds = Number(value.slice(5));
        if (Number.isFinite(seconds))
            return new Date(seconds * 1000).toLocaleString();
    }
    return value;
}
export function yesNoUnknown(value) {
    return value === null ? "不可用" : value ? "是" : "否";
}

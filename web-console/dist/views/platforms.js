import { cell, formatMarker, stateLabel, yesNoUnknown } from "../dom.js";
export function renderPlatforms(platforms) {
    const body = document.getElementById("platform-body");
    const capabilityBody = document.getElementById("capability-body");
    if (!(body instanceof HTMLTableSectionElement) || !(capabilityBody instanceof HTMLTableSectionElement))
        return;
    body.replaceChildren(...platforms.map(platformRow));
    capabilityBody.replaceChildren(...platforms.flatMap(capabilityRows));
}
function platformRow(platform) {
    const row = document.createElement("tr");
    row.append(cell(platform.label), cell(yesNoUnknown(platform.configured)), cell(yesNoUnknown(platform.enabled)), cell(stateLabel(platform.state), `state state-${platform.state}`), cell(formatMarker(platform.lastEventAt)), cell(platform.lastErrorSummary ?? "无"));
    return row;
}
function capabilityRows(platform) {
    return [
        capabilityRow(platform.label, "接收", platform.capabilities.inbound),
        capabilityRow(platform.label, "发送", platform.capabilities.outbound),
    ];
}
function capabilityRow(platformLabel, direction, capabilities) {
    const row = document.createElement("tr");
    row.append(cell(platformLabel), cell(direction), capabilityCell(capabilities.text), capabilityCell(capabilities.markdown), capabilityCell(capabilities.image), capabilityCell(capabilities.file), capabilityCell(capabilities.mixedMessage), capabilityCell(capabilities.streaming));
    return row;
}
function capabilityCell(value) {
    return cell(stateLabel(value), `state state-${value}`);
}

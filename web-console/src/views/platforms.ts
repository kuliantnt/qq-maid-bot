import { cell, formatMarker, stateLabel, yesNoUnknown } from "../dom.js";
import type { PlatformStatus } from "../types.js";

export function renderPlatforms(platforms: PlatformStatus[]): void {
  const body = document.getElementById("platform-body");
  const capabilityBody = document.getElementById("capability-body");
  if (!(body instanceof HTMLTableSectionElement) || !(capabilityBody instanceof HTMLTableSectionElement)) return;
  body.replaceChildren(...platforms.map(platformRow));
  capabilityBody.replaceChildren(...platforms.map(capabilityRow));
}

function platformRow(platform: PlatformStatus): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    cell(platform.label),
    cell(yesNoUnknown(platform.configured)),
    cell(yesNoUnknown(platform.enabled)),
    cell(stateLabel(platform.state), `state state-${platform.state}`),
    cell(formatMarker(platform.lastEventAt)),
    cell(platform.lastErrorSummary ?? "无"),
  );
  return row;
}

function capabilityRow(platform: PlatformStatus): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    cell(platform.label),
    capabilityCell(platform.capabilities.text),
    capabilityCell(platform.capabilities.markdown),
    capabilityCell(platform.capabilities.image),
    capabilityCell(platform.capabilities.file),
    capabilityCell(platform.capabilities.mixedMessage),
    capabilityCell(platform.capabilities.streaming),
  );
  return row;
}

function capabilityCell(value: string): HTMLTableCellElement {
  return cell(stateLabel(value), `state state-${value}`);
}

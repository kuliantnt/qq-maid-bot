import { cell, formatMarker, stateLabel, yesNoUnknown } from "../dom.js";
import type { CapabilityScopeStatus, PlatformStatus } from "../types.js";

export function renderPlatforms(platforms: PlatformStatus[]): void {
  const body = document.getElementById("platform-body");
  const capabilityBody = document.getElementById("capability-body");
  if (!(body instanceof HTMLTableSectionElement) || !(capabilityBody instanceof HTMLTableSectionElement)) return;
  body.replaceChildren(...platforms.map(platformRow));
  capabilityBody.replaceChildren(
    ...platforms.flatMap((platform) =>
      platform.capabilityScopes.flatMap((scope) => capabilityRows(platform.label, scope)),
    ),
  );
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

function capabilityRows(platformLabel: string, scope: CapabilityScopeStatus): HTMLTableRowElement[] {
  return [
    capabilityRow(platformLabel, scope.label, "接收", scope.capabilities.inbound),
    capabilityRow(platformLabel, scope.label, "发送", scope.capabilities.outbound),
  ];
}

function capabilityRow(
  platformLabel: string,
  scopeLabel: string,
  direction: string,
  capabilities: CapabilityScopeStatus["capabilities"]["inbound"],
): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    cell(platformLabel),
    cell(scopeLabel),
    cell(direction),
    capabilityCell(capabilities.text),
    capabilityCell(capabilities.markdown),
    capabilityCell(capabilities.image),
    capabilityCell(capabilities.file),
    capabilityCell(capabilities.mixedMessage),
    capabilityCell(capabilities.streaming),
  );
  return row;
}

function capabilityCell(value: string): HTMLTableCellElement {
  return cell(stateLabel(value), `state state-${value}`);
}

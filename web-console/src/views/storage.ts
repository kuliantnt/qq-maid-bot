import { cell, stateLabel, yesNoUnknown } from "../dom.js";
import type { StorageStatus } from "../types.js";

export function renderStorage(storage: StorageStatus[]): void {
  const body = document.getElementById("storage-body");
  if (!(body instanceof HTMLTableSectionElement)) return;
  body.replaceChildren(...storage.map(storageRow));
}

function storageRow(item: StorageStatus): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    cell(item.label),
    cell(item.pathSummary),
    cell(stateLabel(item.state), `state state-${item.state}`),
    cell(yesNoUnknown(item.exists)),
    cell(yesNoUnknown(item.readable)),
    cell(yesNoUnknown(item.writable)),
    cell(item.schemaSummary ?? "不适用"),
  );
  return row;
}

import { cell, stateLabel, yesNoUnknown } from "../dom.js";
export function renderStorage(storage) {
    const body = document.getElementById("storage-body");
    if (!(body instanceof HTMLTableSectionElement))
        return;
    body.replaceChildren(...storage.map(storageRow));
}
function storageRow(item) {
    const row = document.createElement("tr");
    row.append(cell(item.label), cell(item.pathSummary), cell(stateLabel(item.state), `state state-${item.state}`), cell(yesNoUnknown(item.exists)), cell(yesNoUnknown(item.readable)), cell(yesNoUnknown(item.writable)), cell(item.errorSummary ? stateLabel(item.errorSummary) : "无"), cell(item.schemaSummary ?? "不适用"));
    return row;
}

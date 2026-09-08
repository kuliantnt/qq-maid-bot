/** 表单只产出已有 models.json 的可选元数据；留空/继承不复制目录默认值。 */
import { array, record, string } from "./fields.js";
export function createModelForm() {
    const element = document.createElement("details");
    const summary = document.createElement("summary");
    summary.textContent = "本地新增 / 编辑模型";
    const hint = document.createElement("p");
    hint.textContent = "留空或选择“继承”会使用目录信息。没有目录信息时保持未知。能力声明仅用于展示。";
    const grid = document.createElement("div");
    grid.className = "model-form-grid";
    element.append(summary, hint, grid);
    const id = input(grid, "模型 ID");
    const display = input(grid, "显示名称（留空继承）");
    const context = input(grid, "上下文窗口（token，留空继承）", "number");
    const output = input(grid, "最大输出（token，留空继承）", "number");
    const status = select(grid, "模型状态", [["", "继承"], ["active", "正式"], ["beta", "测试版"], ["deprecated", "已弃用"]]);
    const enabled = select(grid, "本地启用状态", [["", "继承（启用）"], ["true", "启用"], ["false", "禁用"]]);
    const inputPrice = input(grid, "输入价格（美元 / 百万 token）", "number");
    inputPrice.step = "any";
    const outputPrice = input(grid, "输出价格（美元 / 百万 token）", "number");
    outputPrice.step = "any";
    const modalities = modalityFields(grid);
    const claims = Object.fromEntries([["reasoning", "推理"], ["tool_calling", "工具调用"], ["vision", "视觉"]].map(([key, label]) => [key, select(grid, `${label} · 声明能力`, [["", "继承"], ["true", "声明支持"], ["false", "未声明支持"]])]));
    const edit = (modelId, value) => {
        id.value = modelId;
        id.readOnly = !!modelId;
        display.value = string(value.display_name);
        context.value = value.context_window == null ? "" : String(value.context_window);
        output.value = value.max_output_tokens == null ? "" : String(value.max_output_tokens);
        status.value = string(value.status);
        enabled.value = value.enabled == null ? "" : String(value.enabled);
        const price = record(value.price);
        inputPrice.value = price.input_per_million_usd == null ? "" : String(price.input_per_million_usd);
        outputPrice.value = price.output_per_million_usd == null ? "" : String(price.output_per_million_usd);
        modalities.set(value.modalities);
        for (const [key, field] of Object.entries(claims)) {
            const advertised = record(record(value.capabilities)[key]).advertised;
            field.value = advertised == null ? "" : String(advertised);
        }
        id.focus();
    };
    return { element, edit, read: () => {
            if (!id.value.trim())
                throw new Error("请填写模型 ID");
            const result = { id: id.value.trim() };
            if (display.value.trim())
                result.display_name = display.value.trim();
            if (context.value)
                result.context_window = numeric(context.value, true);
            if (output.value)
                result.max_output_tokens = numeric(output.value, true);
            if (status.value)
                result.status = status.value;
            if (enabled.value)
                result.enabled = enabled.value === "true";
            if (inputPrice.value || outputPrice.value)
                result.price = {
                    input_per_million_usd: inputPrice.value ? numeric(inputPrice.value, false) : null,
                    output_per_million_usd: outputPrice.value ? numeric(outputPrice.value, false) : null,
                };
            const value = modalities.get();
            if (value)
                result.modalities = value;
            const capabilities = {};
            for (const [key, field] of Object.entries(claims))
                if (field.value)
                    capabilities[key] = { advertised: field.value === "true" };
            if (Object.keys(capabilities).length)
                result.capabilities = capabilities;
            return result;
        } };
}
function numeric(value, tokens) {
    const number = Number(value);
    if (!Number.isFinite(number) || number < (tokens ? 1 : 0) || (tokens && !Number.isSafeInteger(number)))
        throw new Error(tokens ? "token 数必须是正整数" : "价格必须是非负数字");
    return number;
}
function input(parent, label, type = "text") {
    const field = document.createElement("input");
    field.type = type;
    if (type === "number")
        field.min = "0";
    labeled(parent, label, field);
    return field;
}
function select(parent, label, options) {
    const field = document.createElement("select");
    for (const [value, text] of options) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = text;
        field.append(option);
    }
    labeled(parent, label, field);
    return field;
}
function labeled(parent, text, field) {
    const label = document.createElement("label");
    label.className = field instanceof HTMLInputElement && field.type === "checkbox" ? "provider-field model-checkbox" : "provider-field";
    label.textContent = text;
    field.setAttribute("aria-label", text);
    label.append(field);
    parent.append(label);
}
function modalityFields(parent) {
    const group = document.createElement("fieldset");
    const legend = document.createElement("legend");
    legend.textContent = "输入 / 输出模态";
    group.append(legend);
    const inherit = input(group, "继承目录模态", "checkbox");
    inherit.checked = true;
    const fields = ["input", "output"].map(direction => {
        const section = document.createElement("div");
        const label = document.createElement("p");
        label.textContent = direction === "input" ? "输入" : "输出";
        section.append(label);
        const controls = ["text", "image", "audio", "video", "pdf"].map((value, index) => {
            const field = input(section, ["文本", "图像", "音频", "视频", "PDF"][index], "checkbox");
            field.value = value;
            field.disabled = true;
            return field;
        });
        group.append(section);
        return { direction, controls };
    });
    const refresh = () => { for (const { controls } of fields)
        for (const field of controls)
            field.disabled = inherit.checked; };
    inherit.onchange = refresh;
    parent.append(group);
    return { set: value => {
            inherit.checked = value == null;
            for (const { direction, controls } of fields)
                for (const field of controls)
                    field.checked = array(record(value)[direction]).includes(field.value);
            refresh();
        }, get: () => inherit.checked ? null : Object.fromEntries(fields.map(({ direction, controls }) => [direction, controls.filter(field => field.checked).map(field => field.value)])) };
}

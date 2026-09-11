import { markdown } from "@codemirror/lang-markdown";
import CodeMirror from "@uiw/react-codemirror";
import { useMutation } from "@tanstack/react-query";
import { useAtomValue } from "jotai";
import { useState } from "react";
import { renderMarkdown } from "../../api.js";
import { Button } from "../../components/ui/button.js";
import { Frame, SectionHeader } from "../../components/ui/frame.js";
import { CONSOLE_THEMES } from "../../theme.js";
import { themePresetAtom } from "../../stores/theme.js";

const MAX_LENGTH = 65_536;

/** Tools 页：Markdown 编辑与后端清理预览。
 * 编辑器使用 CodeMirror 6；预览 HTML 只允许来自 Rust 后端 ammonia 清理结果
 * （DESIGN.md §9 安全边界），不在浏览器端复制 sanitization 逻辑。 */
export function MarkdownPage() {
  const preset = useAtomValue(themePresetAtom);
  const colorScheme = CONSOLE_THEMES[preset].colorScheme;
  const [source, setSource] = useState("");
  const [sanitizedHtml, setSanitizedHtml] = useState("");
  const [error, setError] = useState<string | null>(null);

  const renderMutation = useMutation({
    mutationFn: () => renderMarkdown(source),
    onSuccess: (html) => {
      setError(null);
      setSanitizedHtml(html);
    },
    onError: (cause) => {
      setSanitizedHtml("");
      setError(cause instanceof Error ? cause.message : "Markdown 渲染失败");
    },
  });

  return (
    <Frame variant="panel" className="animate-page-in">
      <SectionHeader
        eyebrow="EDITOR / SANITIZED OUTPUT"
        title="Markdown 预览"
        lede="编辑器只提交当前文本；预览内容由后端解析并清理后再显示，不在浏览器端复制 sanitization 逻辑。"
        meta={<span className="font-mono text-[0.66rem] tracking-widest text-muted uppercase">Ctrl / ⌘ + Enter</span>}
      />
      <div className="mt-4 grid gap-4 lg:grid-cols-2">
        <div className="console-frame flex flex-col p-3">
          <div className="mb-2 flex items-center justify-between">
            <label htmlFor="markdown-input" className="text-sm font-semibold text-ink">Markdown 输入</label>
            <span className="font-mono text-[0.66rem] text-muted">最多 {MAX_LENGTH.toLocaleString()} 字符</span>
          </div>
          <div onKeyDown={(event) => {
            if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
              event.preventDefault();
              renderMutation.mutate();
            }
          }}
          >
            <CodeMirror
              id="markdown-input"
              value={source}
              height="360px"
              theme={colorScheme === "dark" ? "dark" : "light"}
              extensions={[markdown()]}
              basicSetup={{ foldGutter: false, searchKeymap: false, highlightSelectionMatches: false }}
              onChange={setSource}
              aria-label="Markdown 输入"
            />
          </div>
          <div className="mt-3 flex items-center gap-3">
            <Button onClick={() => renderMutation.mutate()} disabled={renderMutation.isPending}>
              {renderMutation.isPending ? "渲染中…" : "渲染预览"}
            </Button>
            <span className="text-xs text-muted">支持 Ctrl / ⌘ + Enter 快捷渲染</span>
          </div>
          {error ? (
            <p role="alert" className="m-0 mt-3 text-sm font-semibold text-error">
              {error}
            </p>
          ) : null}
        </div>
        <div className="console-frame flex flex-col p-3">
          <div className="mb-2 flex items-center justify-between">
            <span className="text-sm font-semibold text-ink">安全预览</span>
            <span className="font-mono text-[0.66rem] text-muted">后端清理结果</span>
          </div>
          {sanitizedHtml ? (
            // 安全：内容仅来自后端 ammonia 清理后的 HTML，禁止将本地输入直接写入 innerHTML。
            <article className="markdown-output min-h-40 flex-1 overflow-auto text-sm leading-relaxed" dangerouslySetInnerHTML={{ __html: sanitizedHtml }} />
          ) : (
            <p className="m-0 flex flex-1 items-center justify-center text-sm text-muted" role="status">
              渲染后在此显示后端清理结果。
            </p>
          )}
        </div>
      </div>
    </Frame>
  );
}

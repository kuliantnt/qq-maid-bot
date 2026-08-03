import type { KnowledgeFileItem, KnowledgeFileListParams, KnowledgeFilePage } from "../../types.js";

export type KnowledgePollingDeps = {
  readonly isVisible: () => boolean;
  readonly setTimeout: (fn: () => void, ms: number) => number;
  readonly clearTimeout: (id: number) => void;
  readonly fetchPage: (params: KnowledgeFileListParams) => Promise<KnowledgeFilePage>;
  readonly onUpdate: (page: KnowledgeFilePage) => void;
  readonly onTransientError: (message: string) => void;
  readonly onTerminalTransition: (message: string) => void;
};

const POLL_INTERVAL_MS = 5_000;
const MAX_CONSECUTIVE_FAILURES = 3;

export class KnowledgePollingController {
  private timerId: number | null = null;
  private params: KnowledgeFileListParams | null = null;
  private items: readonly KnowledgeFileItem[] = [];
  private failures = 0;
  private generation = 0;

  constructor(private readonly deps: KnowledgePollingDeps) {}

  start(params: KnowledgeFileListParams): void {
    this.params = { ...params };
    this.failures = 0;
    this.resetTimer();
  }

  updateParams(params: KnowledgeFileListParams): void {
    this.params = { ...params };
    this.resetTimer();
  }

  stop(): void {
    this.generation += 1;
    if (this.timerId !== null) this.deps.clearTimeout(this.timerId);
    this.timerId = null;
  }

  setPages(items: readonly KnowledgeFileItem[]): void {
    this.items = [...items];
  }

  hasActive(): boolean {
    return this.items.some((item) => item.status === "pending" || item.status === "processing");
  }

  notifyChange(): void {
    this.resetTimer();
  }

  private resetTimer(): void {
    this.stop();
    if (this.params !== null && this.hasActive()) this.schedule();
  }

  private schedule(): void {
    this.timerId = this.deps.setTimeout(() => void this.tick(), POLL_INTERVAL_MS);
  }

  private async tick(): Promise<void> {
    this.timerId = null;
    if (!this.hasActive() || this.params === null) return;
    if (!this.deps.isVisible()) {
      this.schedule();
      return;
    }
    const generation = ++this.generation;
    try {
      const page = await this.deps.fetchPage(this.params);
      if (generation !== this.generation) return;
      this.reportTerminalTransitions(page.items);
      this.items = [...page.items];
      this.failures = 0;
      this.deps.onUpdate(page);
      if (this.hasActive()) this.schedule();
    } catch (cause) {
      if (generation !== this.generation) return;
      this.failures += 1;
      this.deps.onTransientError("状态刷新失败");
      if (this.failures >= MAX_CONSECUTIVE_FAILURES) {
        this.stop();
        this.deps.onTransientError("状态刷新多次失败，请手动刷新");
      } else {
        this.schedule();
      }
    }
  }

  private reportTerminalTransitions(nextItems: readonly KnowledgeFileItem[]): void {
    const previous = new Map(this.items.map((item) => [item.file_id, item.status]));
    for (const item of nextItems) {
      const status = previous.get(item.file_id);
      if ((status === "pending" || status === "processing") && item.status === "ready") {
        this.deps.onTerminalTransition("文件处理完成");
      }
      if ((status === "pending" || status === "processing") && item.status === "failed") {
        this.deps.onTerminalTransition("文件处理失败");
      }
    }
  }
}

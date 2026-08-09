const POLL_INTERVAL_MS = 5_000;
const MAX_CONSECUTIVE_FAILURES = 3;
export class KnowledgePollingController {
    deps;
    timerId = null;
    params = null;
    items = [];
    failures = 0;
    generation = 0;
    constructor(deps) {
        this.deps = deps;
    }
    start(params) {
        this.params = { ...params };
        this.failures = 0;
        this.resetTimer();
    }
    updateParams(params) {
        this.params = { ...params };
        this.resetTimer();
    }
    stop() {
        this.generation += 1;
        if (this.timerId !== null)
            this.deps.clearTimeout(this.timerId);
        this.timerId = null;
    }
    setPages(items) {
        this.items = [...items];
    }
    hasActive() {
        return this.items.some((item) => item.status === "pending" || item.status === "processing");
    }
    notifyChange() {
        this.resetTimer();
    }
    resetTimer() {
        this.stop();
        if (this.params !== null && this.hasActive())
            this.schedule();
    }
    schedule() {
        this.timerId = this.deps.setTimeout(() => void this.tick(), POLL_INTERVAL_MS);
    }
    async tick() {
        this.timerId = null;
        if (!this.hasActive() || this.params === null)
            return;
        if (!this.deps.isVisible()) {
            this.schedule();
            return;
        }
        const generation = ++this.generation;
        try {
            const pages = await this.deps.fetchPages(this.params, this.pageCount());
            if (generation !== this.generation)
                return;
            if (pages.length === 0) {
                if (this.hasActive())
                    this.schedule();
                return;
            }
            const nextItems = pages.flatMap((page) => page.items);
            this.reportTerminalTransitions(nextItems);
            this.items = nextItems;
            this.failures = 0;
            this.deps.onUpdate(pages);
            if (this.hasActive())
                this.schedule();
        }
        catch (cause) {
            if (generation !== this.generation)
                return;
            this.failures += 1;
            this.deps.onTransientError("状态刷新失败");
            if (this.failures >= MAX_CONSECUTIVE_FAILURES) {
                this.stop();
                this.deps.onTransientError("状态刷新多次失败，请手动刷新");
            }
            else {
                this.schedule();
            }
        }
    }
    pageCount() {
        return Math.max(1, this.params?.page ?? 1);
    }
    reportTerminalTransitions(nextItems) {
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

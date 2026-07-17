//! 进程内 Ops 长任务注册表。
//!
//! 运行期只保存取消所需的句柄与脱敏作用域。完成记录有数量和时间双重上限；机器人
//! 重启后该内存状态会丢失，但 SQLite 入站领取仍会阻止同一事件再次执行。

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::watch;

const RECENT_TASK_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_RECENT_TASKS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskScope {
    pub platform: String,
    pub account_id: Option<String>,
    pub target_type: String,
    pub target_id: String,
    /// 私聊按发起人不可逆摘要再隔离；群聊为 None，允许同群可信 owner/admin 管理。
    pub private_actor_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagedTaskStatus {
    Running,
    Cancelling,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    SpawnFailed,
}

impl ManagedTaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Cancelling => "正在取消",
            Self::Succeeded => "成功",
            Self::Failed => "执行失败",
            Self::TimedOut => "执行超时",
            Self::Cancelled => "已取消",
            Self::SpawnFailed => "启动失败",
        }
    }
}

pub(super) struct NewManagedTask {
    pub task_id: String,
    pub inbound_key: String,
    pub command_type: String,
    pub command_name: String,
    pub scope: TaskScope,
    pub cancellable: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ManagedTaskView {
    pub task_id: String,
    pub command_type: String,
    pub command_name: String,
    pub status: ManagedTaskStatus,
    pub elapsed: Duration,
    pub cancellable: bool,
}

pub(super) enum RegisterOutcome {
    Registered {
        cancellation: watch::Receiver<bool>,
        process_id: Arc<Mutex<Option<u32>>>,
    },
    Existing(String),
    AtCapacity,
    TaskIdCollision,
}

pub(super) enum CancelOutcome {
    Cancelling,
    Finished(ManagedTaskStatus),
    NotCancellable,
    NotFound,
}

#[derive(Clone, Default)]
pub struct OpsTaskRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    active: HashMap<String, ActiveTask>,
    recent: VecDeque<RecentTask>,
}

struct ActiveTask {
    inbound_key: String,
    command_type: String,
    command_name: String,
    scope: TaskScope,
    started_at: Instant,
    status: ManagedTaskStatus,
    cancellable: bool,
    cancellation: watch::Sender<bool>,
    #[allow(dead_code)]
    process_id: Arc<Mutex<Option<u32>>>,
}

struct RecentTask {
    task_id: String,
    scope: TaskScope,
    status: ManagedTaskStatus,
    finished_at: Instant,
}

impl OpsTaskRegistry {
    pub(super) fn register_codex(
        &self,
        task: NewManagedTask,
        max_concurrent_tasks: usize,
    ) -> RegisterOutcome {
        let mut state = self.inner.lock().unwrap();
        prune_recent(&mut state);
        if let Some((task_id, _)) = state
            .active
            .iter()
            .find(|(_, active)| active.inbound_key == task.inbound_key)
        {
            return RegisterOutcome::Existing(task_id.clone());
        }
        if state.active.contains_key(&task.task_id)
            || state
                .recent
                .iter()
                .any(|recent| recent.task_id == task.task_id)
        {
            return RegisterOutcome::TaskIdCollision;
        }
        let running_codex = state
            .active
            .values()
            .filter(|active| active.command_type == "codex")
            .count();
        if running_codex >= max_concurrent_tasks {
            return RegisterOutcome::AtCapacity;
        }
        let (cancellation, receiver) = watch::channel(false);
        let process_id = Arc::new(Mutex::new(None));
        state.active.insert(
            task.task_id,
            ActiveTask {
                inbound_key: task.inbound_key,
                command_type: task.command_type,
                command_name: task.command_name,
                scope: task.scope,
                started_at: Instant::now(),
                status: ManagedTaskStatus::Running,
                cancellable: task.cancellable,
                cancellation,
                process_id: process_id.clone(),
            },
        );
        RegisterOutcome::Registered {
            cancellation: receiver,
            process_id,
        }
    }

    pub(super) fn remove_unstarted(&self, task_id: &str) {
        self.inner.lock().unwrap().active.remove(task_id);
    }

    pub(super) fn finish(&self, task_id: &str, status: ManagedTaskStatus) {
        let mut state = self.inner.lock().unwrap();
        if let Some(active) = state.active.remove(task_id) {
            state.recent.push_back(RecentTask {
                task_id: task_id.to_owned(),
                scope: active.scope,
                status,
                finished_at: Instant::now(),
            });
        }
        prune_recent(&mut state);
    }

    pub(super) fn list(&self, scope: &TaskScope) -> Vec<ManagedTaskView> {
        let mut state = self.inner.lock().unwrap();
        prune_recent(&mut state);
        let mut tasks = state
            .active
            .iter()
            .filter(|(_, active)| active.scope == *scope)
            .map(|(task_id, active)| ManagedTaskView {
                task_id: task_id.clone(),
                command_type: active.command_type.clone(),
                command_name: active.command_name.clone(),
                status: active.status,
                elapsed: active.started_at.elapsed(),
                cancellable: active.cancellable,
            })
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        tasks
    }

    pub(super) fn cancel(&self, task_id: &str, scope: &TaskScope) -> CancelOutcome {
        let mut state = self.inner.lock().unwrap();
        prune_recent(&mut state);
        if let Some(active) = state.active.get_mut(task_id) {
            if active.scope != *scope {
                return CancelOutcome::NotFound;
            }
            if !active.cancellable {
                return CancelOutcome::NotCancellable;
            }
            if active.status == ManagedTaskStatus::Cancelling {
                return CancelOutcome::Cancelling;
            }
            active.status = ManagedTaskStatus::Cancelling;
            let _ = active.cancellation.send(true);
            return CancelOutcome::Cancelling;
        }
        if let Some(recent) = state
            .recent
            .iter()
            .find(|recent| recent.task_id == task_id && recent.scope == *scope)
        {
            return CancelOutcome::Finished(recent.status);
        }
        CancelOutcome::NotFound
    }
}

fn prune_recent(state: &mut RegistryState) {
    while state
        .recent
        .front()
        .is_some_and(|task| task.finished_at.elapsed() > RECENT_TASK_TTL)
    {
        state.recent.pop_front();
    }
    while state.recent.len() > MAX_RECENT_TASKS {
        state.recent.pop_front();
    }
}

//! RSS 业务域。
//!
//! Tool、Feed 拉取解析、订阅存储、去重调度和主动推送统一收口在这里；
//! respond flow 只负责命令入口，通用 storage 只保留数据库基础设施与 migration 聚合。

pub mod feed;
pub mod scheduler;
pub mod storage;
mod tool;

pub use feed::{RssFetchConfig, RssFetcher};
pub use scheduler::{RssScheduler, RssSchedulerConfig};
pub use storage::*;
pub use tool::{RssManageSubscriptionsTool, RssRecentItemsTool};

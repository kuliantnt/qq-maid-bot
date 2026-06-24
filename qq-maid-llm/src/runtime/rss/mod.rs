//! RSS 订阅运行时模块。
//!
//! 订阅管理、Feed 拉取解析、去重调度和主动推送都收口在这里，
//! respond flow 只负责命令入口，storage 层只负责 SQLite 持久化。

pub mod feed;
pub mod scheduler;

pub use crate::storage::rss::*;
pub use feed::{RssFetchConfig, RssFetcher};
pub use scheduler::{RssScheduler, RssSchedulerConfig};

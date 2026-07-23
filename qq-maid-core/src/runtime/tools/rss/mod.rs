//! RSS 业务域。
//!
//! Tool、Feed 拉取解析、订阅存储、去重调度和主动推送统一收口在这里；
//! respond flow 只负责命令入口，通用 storage 只保留数据库基础设施与 migration 聚合。

pub mod feed;
pub mod scheduler;
pub(crate) mod status;
pub mod storage;
mod tool;

pub use feed::{RssFetchConfig, RssFetcher};
pub use scheduler::{RssScheduler, RssSchedulerConfig};
// RSS storage 已经提供领域级数据模型；显式列出导出面，避免内部持久化类型随 wildcard
// 导出向 Runtime 扩散。
pub use storage::{
    RSS_ITEM_STATES_SCHEMA, RSS_LEGACY_SEEN_ITEMS_MIGRATION, RSS_MIGRATIONS,
    RSS_PENDING_REBASELINE_MIGRATION, RSS_SUBSCRIPTIONS_SCHEMA, RSS_TITLE_SANITIZE_MIGRATION,
    RssFeedItem, RssPendingItem, RssRecentItem, RssStore, RssStoreError, RssSubscription,
    RssTarget, RssTargetType,
};
pub use tool::{RssManageSubscriptionsTool, RssRecentItemsTool};

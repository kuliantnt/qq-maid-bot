//! 请求响应路由与分派。
//!
//! 本模块是 LLM 响应的入口层，负责接收外部（HTTP facade 或内部子 flow）
//! 发来的 `RespondRequest`，根据请求类型和会话状态将其分派到对应的子处理
//! 模块（聊天、翻译、待办、记忆、天气、搜索、会话管理），最终返回 `RespondResponse`。

use crate::{
    error::LlmError,
    provider::DynLlmProvider,
    runtime::{
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        prompt::PromptConfig,
        query::DynQueryExecutor,
        rss::{RssFetcher, RssStore},
        session::{SessionMeta, SessionStore},
        todo::TodoStore,
        train::DynTrainExecutor,
        translation::TranslationService,
        weather::DynWeatherExecutor,
    },
};

mod types;
pub use types::{
    ChatResponse, RespondPurpose, RespondRequest, RespondResponse, RespondStream,
    RespondStreamEvent, RespondTransport,
};

mod chat_flow;
mod command_render;
mod common;
mod help;
mod llm_service;
mod markdown_strip;
mod memory_flow;
mod pending;
mod rss_flow;
mod search_flow;
mod session_flow;
#[cfg(test)]
mod tests;
mod title;
mod todo_flow;
mod train_flow;
mod translation_flow;
mod weather_flow;

use common::{clean_string, session_error};

/// `RustRespondService` 需要的持久化存储集合。
///
/// 这些 store 生命周期一致，收拢后可减少构造函数参数，同时不改变各业务 flow 的边界。
#[derive(Clone)]
pub struct RespondStores {
    /// 长期记忆存储
    pub memory_store: MemoryStore,
    /// 会话记录存储
    pub session_store: SessionStore,
    /// 待办事项存储
    pub todo_store: TodoStore,
    /// RSS 订阅存储
    pub rss_store: RssStore,
}

/// 响应服务外部执行器集合。
///
/// 将查询、天气、列车等执行器收拢为一个参数对象，
/// 减少 `RustRespondService::new` 的构造函数参数数量。
#[derive(Clone)]
pub struct RespondExecutors {
    /// 联网查询执行器
    pub query_executor: DynQueryExecutor,
    /// 天气查询执行器
    pub weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器
    pub train_executor: DynTrainExecutor,
}

/// `RustRespondService` 的可选模型和输出配置。
#[derive(Clone)]
pub struct RespondServiceOptions {
    /// 标题生成专用模型（可选）
    pub title_model: Option<String>,
    /// 待办解析专用模型（可选）
    pub todo_model: Option<String>,
    /// 记忆草稿专用模型（可选）
    pub memory_model: Option<String>,
    /// 上下文压缩专用模型（可选）
    pub compact_model: Option<String>,
    /// 翻译专用模型（可选）；未配置时沿用主 provider 模型。
    pub translation_model: Option<String>,
    /// HTTP 输出模式（final / streaming）
    pub send_mode: String,
    /// RSS 摘要最大字符数
    pub rss_summary_max_chars: usize,
    /// RSS 去重记录保留数量
    pub rss_seen_retention: usize,
}

/// Rust 原生实现的响应服务。
///
/// 聚合所有外部依赖（LLM Provider、会话存储、记忆存储、待办存储等），
/// 提供统一的 `respond` 入口点，将请求按业务语义分派到各子处理模块。
#[derive(Clone)]
pub struct RustRespondService {
    /// LLM 提供商（支持流式 / 非流式聊天）
    provider: DynLlmProvider,
    /// 联网查询执行器
    query_executor: DynQueryExecutor,
    /// 天气查询执行器
    weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器
    train_executor: DynTrainExecutor,
    /// 长期记忆存储
    memory_store: MemoryStore,
    /// 会话记录存储
    session_store: SessionStore,
    /// 待办事项存储
    todo_store: TodoStore,
    /// RSS 订阅存储
    rss_store: RssStore,
    /// RSS / Atom 拉取解析器
    rss_fetcher: RssFetcher,
    /// 本地 Markdown 知识检索索引
    knowledge_index: KnowledgeIndex,
    /// 共享翻译执行器；命令和 RSS 共用同一套 provider 调用逻辑。
    translation_service: TranslationService,
    /// 系统提示词配置
    prompt_config: PromptConfig,
    /// 标题自动生成专用模型名（若指定则覆盖默认模型）
    title_model: Option<String>,
    /// 待办解析专用模型名
    todo_model: Option<String>,
    /// 记忆草稿专用模型名
    memory_model: Option<String>,
    /// 会话上下文压缩专用模型名
    compact_model: Option<String>,
    /// HTTP 输出模式（final / streaming）
    send_mode: String,
    /// RSS 摘要最大字符数
    rss_summary_max_chars: usize,
    /// 每个订阅保留的去重指纹数量
    rss_seen_retention: usize,
}

impl RustRespondService {
    /// 构造 `RustRespondService`。
    ///
    /// 所有依赖均为必需注入，不存在默认值或 fallback 构造。
    pub fn new(
        provider: DynLlmProvider,
        executors: RespondExecutors,
        stores: RespondStores,
        rss_fetcher: RssFetcher,
        knowledge_index: KnowledgeIndex,
        prompt_config: PromptConfig,
        options: RespondServiceOptions,
    ) -> Self {
        let translation_service =
            TranslationService::new(provider.clone(), options.translation_model);
        Self {
            provider,
            query_executor: executors.query_executor,
            weather_executor: executors.weather_executor,
            train_executor: executors.train_executor,
            memory_store: stores.memory_store,
            session_store: stores.session_store,
            todo_store: stores.todo_store,
            rss_store: stores.rss_store,
            rss_fetcher,
            knowledge_index,
            translation_service,
            prompt_config,
            title_model: options.title_model,
            todo_model: options.todo_model,
            memory_model: options.memory_model,
            compact_model: options.compact_model,
            send_mode: options.send_mode,
            rss_summary_max_chars: options.rss_summary_max_chars,
            rss_seen_retention: options.rss_seen_retention,
        }
    }

    /// 统一的请求响应入口。
    ///
    /// 分派顺序：
    /// 1. 检查会话中是否有**待处理操作**（pending operation），若有则优先处理。
    /// 2. 解析是否为**会话管理指令**（`/new`, `/clear`, `/state` 等）。
    /// 3. 获取或创建活跃会话。
    /// 4. 检查是否为**翻译命令**。
    /// 5. 检查是否为**天气查询命令**。
    /// 6. 检查是否为**联网搜索命令**。
    /// 7. 检查是否为**待办相关操作**。
    /// 8. 检查是否为**长期记忆操作**。
    /// 9. 兜底：进入**普通聊天**处理流程。
    pub async fn respond(&self, req: RespondRequest) -> Result<RespondResponse, LlmError> {
        match self.respond_transport(req, false).await? {
            RespondTransport::Json(response) => Ok(*response),
            RespondTransport::Stream(_) => unreachable!("respond() must never return stream"),
        }
    }

    /// 面向 HTTP 层的统一入口。
    ///
    /// `allow_streaming` 由路由层控制；即使配置开启 streaming，调用方不允许时
    /// 也会继续返回原本的完整 JSON 响应，保证内部调用行为不变。
    pub async fn respond_transport(
        &self,
        req: RespondRequest,
        allow_streaming: bool,
    ) -> Result<RespondTransport, LlmError> {
        let user_text = req.effective_user_text();
        let reply_text = req.reply_text.clone();
        let meta = SessionMeta::new(
            req.scope_key.clone(),
            req.user_id.clone(),
            req.group_id.clone(),
            req.guild_id.clone(),
            req.channel_id.clone(),
            clean_string(req.platform.clone()).unwrap_or_else(|| "qq".to_owned()),
        );

        // 尝试获取当前会话的活跃记录
        let mut active_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;

        // 若用户输入不是跳等待办的会话指令，则先检查是否有待处理操作（pending）
        let bypass_pending_for_session_command =
            session_flow::parse_pending_bypass_session_command(&user_text).is_some();
        if !bypass_pending_for_session_command
            && let Some(session) = active_session.as_mut()
            && let Some(response) = self
                .handle_pending_operation(&user_text, &meta, session)
                .await?
        {
            return Ok(json_transport(response));
        }

        // 检查是否为会话管理指令（/new, /clear, /state 等）
        if let Some(command) = session_flow::parse_session_command(&user_text) {
            return Ok(json_transport(
                self.handle_session_command(command, &meta).await?,
            ));
        }

        // 确保存在活跃会话（无则创建）
        let mut session = match active_session {
            Some(session) => session,
            None => self
                .session_store
                .get_or_create_active(&meta)
                .map_err(session_error)?,
        };

        // 检查是否为翻译指令（如 "/翻译 文本"、"/翻译日语 文本"）
        if let Some(command) = translation_flow::parse_translation_command(&user_text) {
            return Ok(json_transport(
                self.handle_translation_command(command, &meta, &user_text, &mut session)
                    .await?,
            ));
        }

        // 检查是否为天气查询指令（如 "/北京天气" 或 "/天气北京"）
        if let Some(command) = weather_flow::parse_weather_command(&user_text) {
            return Ok(json_transport(
                self.handle_weather_command(command, &user_text, &mut session)
                    .await?,
            ));
        }

        // 检查是否为列车时刻查询指令（如 "/火车 G1 明天"）
        if let Some(command) = train_flow::parse_train_command(&user_text) {
            return Ok(json_transport(
                self.handle_train_command(command, &user_text, &mut session)
                    .await?,
            ));
        }

        // 检查是否为联网搜索指令（如 "/查 关键词"）
        if let Some(command) = search_flow::parse_web_search_command(&user_text) {
            if allow_streaming && self.send_mode.eq_ignore_ascii_case("streaming") {
                return self
                    .handle_web_search_command_stream(command, &mut session)
                    .await;
            }
            return Ok(json_transport(
                self.handle_web_search_command(command, &mut session)
                    .await?,
            ));
        }

        // 检查是否为 RSS 订阅指令（如 "/rss add ..." 或 "/订阅"）
        if let Some(response) = self
            .handle_rss_flow(&user_text, &meta, &mut session)
            .await?
        {
            return Ok(json_transport(response));
        }

        // 检查是否为待办相关操作（新增、查看、完成、编辑、删除等）
        if let Some(response) = self
            .handle_todo_flow(&user_text, &meta, &mut session, reply_text.as_deref())
            .await?
        {
            return Ok(json_transport(response));
        }

        // 检查是否为长期记忆相关操作（记忆新增、查看、更新、删除等）
        if let Some(response) = self
            .handle_memory_flow(&user_text, &meta, &mut session)
            .await?
        {
            return Ok(json_transport(response));
        }

        // 兜底：进入普通 LLM 聊天流程
        Ok(json_transport(
            self.handle_chat(req, user_text, meta, session).await?,
        ))
    }
}

fn json_transport(response: RespondResponse) -> RespondTransport {
    RespondTransport::Json(Box::new(response))
}

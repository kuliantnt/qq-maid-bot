use super::*;

pub(crate) struct EmptyWebSearchExecutor;

#[async_trait::async_trait]
impl WebSearchExecutor for EmptyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::provider("query unused", "query"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-query"
    }
}

#[derive(Clone, Default)]
pub(crate) struct MockWebSearchExecutor {
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
}

impl MockWebSearchExecutor {
    pub(crate) fn requests(&self) -> Vec<WebSearchRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl WebSearchExecutor for MockWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(WebSearchOutcome {
            answer: format!("web answer: {}", req.query),
            sources: Vec::new(),
            provider: "mock-query".to_owned(),
            elapsed_ms: 1,
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
    }
}

pub(crate) struct EmptyWeatherExecutor;

#[async_trait::async_trait]
impl WeatherExecutor for EmptyWeatherExecutor {
    async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        Err(LlmError::provider("weather unused", "weather"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-weather"
    }
}

pub(crate) struct EmptyTrainExecutor;

#[async_trait::async_trait]
impl TrainExecutor for EmptyTrainExecutor {
    async fn query_train_schedule(
        &self,
        _req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        Err(LlmError::provider("train unused", "train"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-train"
    }
}

pub(crate) struct EmptyRadarExecutor;

#[async_trait::async_trait]
impl RadarExecutor for EmptyRadarExecutor {
    async fn radar(&self, _target: RadarTarget) -> Result<RadarSnapshot, LlmError> {
        Err(LlmError::provider("radar unused", "radar"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-radar"
    }
}

pub(crate) fn private_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        message_id: Some("test-private-message".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        addressed_to_bot: false,
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    }
}

pub(crate) fn private_scope() -> &'static str {
    "platform:qq_official:account:app-1:private:u1"
}

pub(crate) fn group_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        message_id: Some("test-group-message".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        addressed_to_bot: false,
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    }
}

pub(crate) fn wechat_service_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        message_id: Some("test-wechat-message".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::WechatService,
        account_id: Some("gh-service".to_owned()),
        actor: CoreActor {
            user_id: Some("openid-u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        addressed_to_bot: false,
        conversation: CoreConversation::ServiceAccount {
            account_id: Some("gh_test".to_owned()),
            peer_id: "openid-u1".to_owned(),
        },
    }
}

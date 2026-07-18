use std::collections::{HashMap, HashSet};

use super::{
    ConfigCenterError, ManagedConfigField, ManagedConfigSensitivity, ManagedConfigValueType,
};
use toml::Value;

#[derive(Debug, Clone)]
pub struct ConfigRegistry {
    fields: Vec<ManagedConfigField>,
    by_key: HashMap<&'static str, usize>,
}

impl ConfigRegistry {
    pub fn new(fields: Vec<ManagedConfigField>) -> Result<Self, ConfigCenterError> {
        let mut by_key = HashMap::new();
        let mut env_names = HashSet::new();
        for (index, field) in fields.iter().enumerate() {
            if !valid_stable_key(field.key) {
                return Err(ConfigCenterError::invalid(format!(
                    "invalid managed config key `{}`",
                    field.key
                )));
            }
            if field.env_name.trim().is_empty() || field.module.trim().is_empty() {
                return Err(ConfigCenterError::invalid(format!(
                    "managed config field `{}` has empty metadata",
                    field.key
                )));
            }
            if by_key.insert(field.key, index).is_some() {
                return Err(ConfigCenterError::invalid(format!(
                    "duplicate managed config key `{}`",
                    field.key
                )));
            }
            for env_name in std::iter::once(&field.env_name).chain(field.env_aliases.iter()) {
                if env_name.trim().is_empty() || !env_names.insert(*env_name) {
                    return Err(ConfigCenterError::invalid(format!(
                        "duplicate or empty managed environment mapping `{env_name}`"
                    )));
                }
            }
            if field.sensitivity == ManagedConfigSensitivity::Secret
                && field.default_value.is_some()
            {
                return Err(ConfigCenterError::invalid(format!(
                    "secret field `{}` must not define a default",
                    field.key
                )));
            }
            if let Some(default) = field.default_value {
                parse_raw_value(field, default)?;
            }
        }
        Ok(Self { fields, by_key })
    }

    pub fn fields(&self) -> &[ManagedConfigField] {
        &self.fields
    }

    pub fn require(&self, key: &str) -> Result<&ManagedConfigField, ConfigCenterError> {
        self.by_key
            .get(key)
            .map(|index| &self.fields[*index])
            .ok_or_else(|| ConfigCenterError::invalid(format!("unknown config field `{key}`")))
    }

    pub fn validate_managed_value(
        &self,
        field: &ManagedConfigField,
        value: &Value,
    ) -> Result<(), ConfigCenterError> {
        if field.sensitivity != ManagedConfigSensitivity::Public || !field.web_editable {
            return Err(ConfigCenterError::invalid(format!(
                "field `{}` cannot be stored in managed TOML",
                field.key
            )));
        }
        match (field.value_type, value) {
            (ManagedConfigValueType::String, Value::String(value)) if !value.trim().is_empty() => {
                validate_field_semantics(field, value)
            }
            (ManagedConfigValueType::Boolean, Value::Boolean(_)) => Ok(()),
            (ManagedConfigValueType::Integer, Value::Integer(value)) => {
                validate_integer_semantics(field, *value)
            }
            (ManagedConfigValueType::StringList, Value::Array(values))
                if values.iter().all(
                    |value| matches!(value, Value::String(item) if !item.trim().is_empty()),
                ) =>
            {
                Ok(())
            }
            _ => Err(ConfigCenterError::invalid(format!(
                "field `{}` has invalid value type",
                field.key
            ))),
        }
    }

    pub fn parse_environment_value(
        &self,
        field: &ManagedConfigField,
        raw: &str,
    ) -> Result<Value, ConfigCenterError> {
        parse_raw_value(field, raw)
    }

    pub fn environment_string(
        &self,
        field: &ManagedConfigField,
        value: &Value,
    ) -> Result<String, ConfigCenterError> {
        self.validate_managed_value(field, value)?;
        Ok(match value {
            Value::String(value) => value.clone(),
            Value::Boolean(value) => value.to_string(),
            Value::Integer(value) => value.to_string(),
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(","),
            _ => unreachable!("value type was validated above"),
        })
    }
}

fn validate_field_semantics(
    field: &ManagedConfigField,
    value: &str,
) -> Result<(), ConfigCenterError> {
    match field.env_name {
        "CHAT_COMMAND_PREFIX" => qq_maid_common::command_prefix::CommandPrefix::parse(value)
            .map(|_| ())
            .map_err(|error| {
                ConfigCenterError::invalid(format!("field `{}` is invalid: {error}", field.key))
            }),
        "OPENAI_API_MODE" => require_choice(field, value, &["auto", "chat_only", "chat-only"]),
        "WECHAT_SERVICE_ENCRYPTION_MODE" => require_choice(field, value, &["plaintext", "aes"]),
        "TODO_DAILY_REMINDER_TIME" => {
            crate::config::DailyReminderTime::parse_config(value, field.env_name)
                .map(|_| ())
                .map_err(|err| ConfigCenterError::invalid(err.to_string()))
        }
        "OPENAI_BASE_URLS" | "DEEPSEEK_BASE_URL" | "BIGMODEL_BASE_URL" | "GEMINI_BASE_URL" => {
            for item in value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
            {
                let url = reqwest::Url::parse(item).map_err(|_| {
                    ConfigCenterError::invalid(format!(
                        "field `{}` must contain valid HTTP(S) URLs",
                        field.key
                    ))
                })?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(ConfigCenterError::invalid(format!(
                        "field `{}` must contain valid HTTP(S) URLs",
                        field.key
                    )));
                }
            }
            Ok(())
        }
        "ONEBOT11_WEBSOCKET_PATH" | "WECHAT_SERVICE_CALLBACK_PATH" if !value.starts_with('/') => {
            Err(ConfigCenterError::invalid(format!(
                "field `{}` must begin with '/'",
                field.key
            )))
        }
        _ => Ok(()),
    }
}

fn validate_integer_semantics(
    field: &ManagedConfigField,
    value: i64,
) -> Result<(), ConfigCenterError> {
    match field.env_name {
        "ONEBOT11_BIND_PORT" | "WECHAT_SERVICE_BIND_PORT" if !(1..=65_535).contains(&value) => {
            Err(ConfigCenterError::invalid(format!(
                "field `{}` must be between 1 and 65535",
                field.key
            )))
        }
        _ => Ok(()),
    }
}

fn require_choice(
    field: &ManagedConfigField,
    value: &str,
    choices: &[&str],
) -> Result<(), ConfigCenterError> {
    let normalized = value.trim().to_ascii_lowercase();
    if choices.contains(&normalized.as_str()) {
        Ok(())
    } else {
        Err(ConfigCenterError::invalid(format!(
            "field `{}` has an unsupported value",
            field.key
        )))
    }
}

fn parse_raw_value(field: &ManagedConfigField, raw: &str) -> Result<Value, ConfigCenterError> {
    match field.value_type {
        ManagedConfigValueType::String => {
            let value = raw.trim();
            if value.is_empty() {
                return Err(ConfigCenterError::invalid(format!(
                    "field `{}` must not be empty",
                    field.key
                )));
            }
            Ok(Value::String(value.to_owned()))
        }
        ManagedConfigValueType::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" | "enabled" => Ok(Value::Boolean(true)),
            "0" | "false" | "off" | "no" | "disabled" | "none" => Ok(Value::Boolean(false)),
            _ => Err(ConfigCenterError::invalid(format!(
                "field `{}` must be a boolean",
                field.key
            ))),
        },
        ManagedConfigValueType::Integer => {
            raw.trim().parse::<i64>().map(Value::Integer).map_err(|_| {
                ConfigCenterError::invalid(format!("field `{}` must be an integer", field.key))
            })
        }
        ManagedConfigValueType::StringList => Ok(Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| Value::String(value.to_owned()))
                .collect(),
        )),
    }
}

fn valid_stable_key(key: &str) -> bool {
    !key.is_empty()
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

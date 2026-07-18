//! 跨 Core / Gateway 共享的受管配置字段元数据。
//!
//! 本模块只描述稳定 key、旧环境变量映射和安全属性，不读取文件、数据库或进程环境。
//! 各业务 crate 只声明自己理解的字段，配置中心再在 Core 中统一编排。

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedConfigValueType {
    String,
    Boolean,
    Integer,
    StringList,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedConfigSensitivity {
    Public,
    Secret,
    Restricted,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedConfigApplyMode {
    Immediate,
    Restart,
}

/// 单个可登记配置字段。
///
/// `key` 是配置中心稳定 key；`env_name` 只用于兼容现有 resolver，不能作为新的领域边界。
#[derive(Debug, Clone, Copy)]
pub struct ManagedConfigField {
    pub key: &'static str,
    pub env_name: &'static str,
    pub env_aliases: &'static [&'static str],
    pub module: &'static str,
    pub value_type: ManagedConfigValueType,
    pub sensitivity: ManagedConfigSensitivity,
    pub apply_mode: ManagedConfigApplyMode,
    pub web_editable: bool,
    pub default_value: Option<&'static str>,
}

impl ManagedConfigField {
    pub const fn public(
        key: &'static str,
        env_name: &'static str,
        module: &'static str,
        value_type: ManagedConfigValueType,
        apply_mode: ManagedConfigApplyMode,
        default_value: Option<&'static str>,
    ) -> Self {
        Self {
            key,
            env_name,
            env_aliases: &[],
            module,
            value_type,
            sensitivity: ManagedConfigSensitivity::Public,
            apply_mode,
            web_editable: true,
            default_value,
        }
    }

    pub const fn secret(
        key: &'static str,
        env_name: &'static str,
        module: &'static str,
        apply_mode: ManagedConfigApplyMode,
    ) -> Self {
        Self {
            key,
            env_name,
            env_aliases: &[],
            module,
            value_type: ManagedConfigValueType::String,
            sensitivity: ManagedConfigSensitivity::Secret,
            apply_mode,
            web_editable: true,
            default_value: None,
        }
    }

    pub const fn restricted(
        key: &'static str,
        env_name: &'static str,
        module: &'static str,
        value_type: ManagedConfigValueType,
        apply_mode: ManagedConfigApplyMode,
        default_value: Option<&'static str>,
    ) -> Self {
        Self {
            key,
            env_name,
            env_aliases: &[],
            module,
            value_type,
            sensitivity: ManagedConfigSensitivity::Restricted,
            apply_mode,
            web_editable: false,
            default_value,
        }
    }

    /// 登记仍由真实 resolver 支持的兼容环境变量名。
    pub const fn with_env_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.env_aliases = aliases;
        self
    }
}

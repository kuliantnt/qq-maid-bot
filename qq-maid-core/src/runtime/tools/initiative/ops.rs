use super::command::{HELP, InitiativeCommand, parse_entries, valid_name};
use crate::runtime::tools::roll::dice::{Roller, csprng_roller};
use qq_maid_common::identity_context::MentionIdentity;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub(crate) struct InitiativeService {
    tables: Arc<Mutex<HashMap<String, Table>>>,
}

#[derive(Clone, Default)]
struct Table {
    entries: Vec<Entry>,
    current: Option<String>,
    round: u64,
    started: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entry {
    name: String,
    value: i32,
    /// 仅当名称由当前玩家的 `/nn` 或平台展示名自动补全时保存，显式命名单位为 None。
    actor: Option<MentionIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InitiativeReply {
    pub(crate) text: String,
    pub(crate) mentions: Vec<MentionIdentity>,
}

impl InitiativeReply {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mentions: Vec::new(),
        }
    }
}

impl InitiativeService {
    #[cfg(test)]
    pub(crate) fn execute(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        name: Option<&str>,
    ) -> String {
        self.execute_reply_with_roller(scope, command, name, None, &mut csprng_roller())
            .map(|reply| reply.text)
            .unwrap_or_else(str::to_owned)
    }

    /// 执行命令并保留结构化玩家身份，供推进回合时生成平台无关 Mention。
    pub(crate) fn execute_for_actor(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        actor: Option<&MentionIdentity>,
    ) -> InitiativeReply {
        let default_name = actor.and_then(actor_display_name);
        self.execute_reply_with_roller(scope, command, default_name, actor, &mut csprng_roller())
            .unwrap_or_else(InitiativeReply::plain)
    }

    #[cfg(test)]
    pub(super) fn execute_with_roller(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        name: Option<&str>,
        roller: &mut impl Roller,
    ) -> Result<String, &'static str> {
        self.execute_reply_with_roller(scope, command, name, None, roller)
            .map(|reply| reply.text)
    }

    fn execute_reply_with_roller(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        default_name: Option<&str>,
        actor: Option<&MentionIdentity>,
        roller: &mut impl Roller,
    ) -> Result<InitiativeReply, &'static str> {
        if scope.trim().is_empty() {
            return Err("缺少会话作用域，无法维护先攻表。");
        }
        match command {
            InitiativeCommand::Help => return Ok(InitiativeReply::plain(HELP)),
            InitiativeCommand::Invalid => return Err(HELP),
            _ => {}
        }
        // 一次批量校验、投骰和更新在同一锁内完成，失败不留下半张先攻表。
        let mut tables = self.tables.lock().map_err(|_| "先攻状态锁异常，请重试。")?;
        let mut table = tables.get(scope).cloned().unwrap_or_default();
        let mut details = Vec::new();
        let mut mentions = Vec::new();
        match command {
            InitiativeCommand::Record(input) | InitiativeCommand::Set(input) => {
                let entries = parse_entries(
                    input,
                    default_name,
                    matches!(command, InitiativeCommand::Set(_)),
                )?;
                for parsed in entries {
                    let result = parsed
                        .expression
                        .roll(roller)
                        .map_err(|_| "先攻投掷失败，本次未更新先攻表。")?;
                    details.push(format!(
                        "{}：{} = {}",
                        parsed.name,
                        result.calculation(),
                        result.total
                    ));
                    // 显式命名会覆盖为手工单位，必须同步清除旧绑定，避免同名 NPC
                    // 继承此前玩家身份或在更新后留下陈旧 Mention。
                    let entry_actor = parsed.binds_actor.then(|| actor.cloned()).flatten();
                    if let Some(entry) = table
                        .entries
                        .iter_mut()
                        .find(|entry| entry.name == parsed.name)
                    {
                        entry.value = result.total;
                        entry.actor = entry_actor;
                    } else {
                        table.entries.push(Entry {
                            name: parsed.name,
                            value: result.total,
                            actor: entry_actor,
                        });
                    }
                }
                if table.entries.len() > 100 {
                    return Err("先攻表最多容纳 100 个单位。");
                }
                // 同值按名称 Unicode 顺序，更新和不同录入顺序均不引入随机并列顺序。
                table
                    .entries
                    .sort_by(|a, b| b.value.cmp(&a.value).then_with(|| a.name.cmp(&b.name)));
                if !table.started {
                    table.current = table.entries.first().map(|entry| entry.name.clone());
                    table.round = 1;
                }
            }
            InitiativeCommand::Delete(names) => {
                table.delete(names)?;
            }
            InitiativeCommand::End => {
                let Some(index) = table.current_index() else {
                    return Err("先攻表为空，请先 /ri 录入单位。");
                };
                table.started = true;
                let next = (index + 1) % table.entries.len();
                if next == 0 {
                    table.round = table.round.saturating_add(1);
                }
                table.current = Some(table.entries[next].name.clone());
                if let Some(actor) = table.entries[next].actor.clone() {
                    mentions.push(actor);
                }
            }
            InitiativeCommand::Clear => {
                table = Table::default();
            }
            InitiativeCommand::List => return Ok(InitiativeReply::plain(table.render())),
            _ => unreachable!(),
        }
        let reply = table.render();
        if table.entries.is_empty() {
            tables.remove(scope);
        } else {
            // 不静默淘汰正在使用的战斗；总体上限防止临时状态无限增长。
            if !tables.contains_key(scope) && tables.len() >= 4096 {
                return Err("临时先攻会话已达上限，请清理不用的先攻表。");
            }
            tables.insert(scope.to_owned(), table);
        }
        if details.is_empty() {
            Ok(InitiativeReply {
                text: reply,
                mentions,
            })
        } else {
            Ok(InitiativeReply {
                text: format!("{}\n{reply}", details.join("\n")),
                mentions,
            })
        }
    }
}

impl Table {
    // 删除当前者沿删除前的环形顺序选择下一存活单位，避免索引位移导致跳人。
    fn delete(&mut self, names: &[String]) -> Result<(), &'static str> {
        if names.len() > 100 || names.iter().any(|n| !valid_name(n)) {
            return Err(HELP);
        }
        if names
            .iter()
            .any(|name| !self.entries.iter().any(|entry| &entry.name == name))
        {
            return Err("有单位不在先攻表中，本次未删除；请用 /init 核对名称。");
        }
        let current_index = self.current_index();
        let next = current_index.and_then(|index| {
            (1..=self.entries.len())
                .map(|offset| (index + offset) % self.entries.len())
                .find(|i| !names.contains(&self.entries[*i].name))
        });
        if self.current.as_ref().is_some_and(|n| names.contains(n))
            && let Some(next) = next
        {
            if self.started && next <= current_index.unwrap_or(0) {
                self.round = self.round.saturating_add(1);
            }
            self.current = Some(self.entries[next].name.clone());
        }
        self.entries.retain(|entry| !names.contains(&entry.name));
        if self.entries.is_empty() {
            *self = Table::default();
        } else if !self.started {
            self.current = Some(self.entries[0].name.clone());
        }
        Ok(())
    }
    fn current_index(&self) -> Option<usize> {
        self.current
            .as_ref()
            .and_then(|name| self.entries.iter().position(|entry| &entry.name == name))
    }
    fn render(&self) -> String {
        if self.entries.is_empty() {
            return "先攻表为空（回合已重置）。".to_owned();
        }
        let mut lines = vec![format!(
            "先攻表 · 第 {} 轮 · 当前：{}",
            self.round,
            self.current.as_deref().unwrap_or("")
        )];
        for (index, entry) in self.entries.iter().enumerate() {
            lines.push(format!(
                "{}. {}：{}{}",
                index + 1,
                entry.name,
                entry.value,
                if self.current.as_ref() == Some(&entry.name) {
                    " ← 当前"
                } else {
                    ""
                }
            ));
        }
        lines.join("\n")
    }
}

fn actor_display_name(actor: &MentionIdentity) -> Option<&str> {
    actor
        .target
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

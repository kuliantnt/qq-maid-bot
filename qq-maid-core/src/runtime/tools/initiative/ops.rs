use super::command::{HELP, InitiativeCommand, parse_entries, valid_name};
use crate::runtime::tools::roll::dice::{Roller, csprng_roller};
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
    entries: Vec<(String, i32)>,
    current: Option<String>,
    round: u64,
    started: bool,
}

impl InitiativeService {
    pub(crate) fn execute(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        name: Option<&str>,
    ) -> String {
        self.execute_with_roller(scope, command, name, &mut csprng_roller())
            .unwrap_or_else(str::to_owned)
    }

    pub(super) fn execute_with_roller(
        &self,
        scope: &str,
        command: &InitiativeCommand,
        name: Option<&str>,
        roller: &mut impl Roller,
    ) -> Result<String, &'static str> {
        if scope.trim().is_empty() {
            return Err("缺少会话作用域，无法维护先攻表。");
        }
        match command {
            InitiativeCommand::Help => return Ok(HELP.to_owned()),
            InitiativeCommand::Invalid => return Err(HELP),
            _ => {}
        }
        // 一次批量校验、投骰和更新在同一锁内完成，失败不留下半张先攻表。
        let mut tables = self.tables.lock().map_err(|_| "先攻状态锁异常，请重试。")?;
        let mut table = tables.get(scope).cloned().unwrap_or_default();
        let mut details = Vec::new();
        match command {
            InitiativeCommand::Record(input) | InitiativeCommand::Set(input) => {
                let entries =
                    parse_entries(input, name, matches!(command, InitiativeCommand::Set(_)))?;
                for (name, expr) in entries {
                    let result = expr
                        .roll(roller)
                        .map_err(|_| "先攻投掷失败，本次未更新先攻表。")?;
                    details.push(format!(
                        "{name}：{} = {}",
                        result.calculation(),
                        result.total
                    ));
                    if let Some(entry) = table.entries.iter_mut().find(|entry| entry.0 == name) {
                        entry.1 = result.total;
                    } else {
                        table.entries.push((name, result.total));
                    }
                }
                if table.entries.len() > 100 {
                    return Err("先攻表最多容纳 100 个单位。");
                }
                // 同值按名称 Unicode 顺序，更新和不同录入顺序均不引入随机并列顺序。
                table
                    .entries
                    .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                if !table.started {
                    table.current = table.entries.first().map(|e| e.0.clone());
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
                table.current = Some(table.entries[next].0.clone());
            }
            InitiativeCommand::Clear => {
                table = Table::default();
            }
            InitiativeCommand::List => return Ok(table.render()),
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
            Ok(reply)
        } else {
            Ok(format!("{}\n{reply}", details.join("\n")))
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
            .any(|n| !self.entries.iter().any(|e| &e.0 == n))
        {
            return Err("有单位不在先攻表中，本次未删除；请用 /init 核对名称。");
        }
        let current_index = self.current_index();
        let next = current_index.and_then(|index| {
            (1..=self.entries.len())
                .map(|offset| (index + offset) % self.entries.len())
                .find(|i| !names.contains(&self.entries[*i].0))
        });
        if self.current.as_ref().is_some_and(|n| names.contains(n))
            && let Some(next) = next
        {
            if self.started && next <= current_index.unwrap_or(0) {
                self.round = self.round.saturating_add(1);
            }
            self.current = Some(self.entries[next].0.clone());
        }
        self.entries.retain(|e| !names.contains(&e.0));
        if self.entries.is_empty() {
            *self = Table::default();
        } else if !self.started {
            self.current = Some(self.entries[0].0.clone());
        }
        Ok(())
    }
    fn current_index(&self) -> Option<usize> {
        self.current
            .as_ref()
            .and_then(|n| self.entries.iter().position(|e| &e.0 == n))
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
        for (index, (name, value)) in self.entries.iter().enumerate() {
            lines.push(format!(
                "{}. {name}：{value}{}",
                index + 1,
                if self.current.as_ref() == Some(name) {
                    " ← 当前"
                } else {
                    ""
                }
            ));
        }
        lines.join("\n")
    }
}

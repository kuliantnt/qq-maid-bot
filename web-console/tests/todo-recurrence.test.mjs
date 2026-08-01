import assert from "node:assert/strict";
import test from "node:test";

import { todoRecurrenceKind } from "../dist/views/todo/todo.js";

test("Todo 间隔重复按单位转换为后端支持的枚举", () => {
  assert.equal(todoRecurrenceKind("none", "day"), "none");
  assert.equal(todoRecurrenceKind("every_n_days", "day"), "every_n_days");
  assert.equal(todoRecurrenceKind("every_n_days", "week"), "every_n_weeks");
  assert.equal(todoRecurrenceKind("every_n_days", "month"), "every_n_months");
  assert.equal(todoRecurrenceKind("every_n_days", "year"), "every_n_years");
  assert.equal(todoRecurrenceKind("every_n_days", "minute"), "every_n_minutes");
  assert.equal(todoRecurrenceKind("every_n_days", "hour"), "every_n_hours");
});

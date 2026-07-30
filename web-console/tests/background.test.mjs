import assert from "node:assert/strict";
import test from "node:test";

import {
  BACKGROUND_MODE_COOKIE,
  BACKGROUND_UNLOCK_COOKIE,
  createBackgroundController,
  installBackgroundConsoleUnlock,
} from "../dist/background.js";

function cookieDocument(initial = "") {
  let value = initial;
  return {
    get cookie() {
      return value;
    },
    set cookie(next) {
      value = value ? `${value}; ${next.split(";")[0]}` : next.split(";")[0];
    },
    read() {
      return value;
    },
  };
}

test("背景默认模式不读取未解锁的特殊模式", () => {
  const cookies = cookieDocument(`${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  assert.equal(controller.current(), "default");
  assert.equal(controller.isUnlocked(), false);
  assert.equal(root.dataset.background, "default");
});

test("控制台解锁会切换特殊背景并写入长期 cookie", () => {
  const cookies = cookieDocument();
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);
  const target = {};
  installBackgroundConsoleUnlock(target, controller);

  assert.equal(target.kuliantnt, "特殊背景已解锁");
  assert.equal(controller.current(), "special");
  assert.equal(controller.isUnlocked(), true);
  assert.equal(root.dataset.background, "special");
  assert.match(cookies.read(), new RegExp(`${BACKGROUND_UNLOCK_COOKIE}=1`));
  assert.match(cookies.read(), new RegExp(`${BACKGROUND_MODE_COOKIE}=special`));
});

test("特殊背景解锁后可以选择回普通背景并在刷新后恢复", () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  controller.select("default");
  assert.equal(controller.current(), "default");
  assert.equal(controller.select("special"), "special");
  assert.equal(createBackgroundController(root, cookies).current(), "special");
});

test("普通背景过渡固定使用默认图，特殊背景按九张图片循环", () => {
  const cookies = cookieDocument(`${BACKGROUND_UNLOCK_COOKIE}=1; ${BACKGROUND_MODE_COOKIE}=special`);
  const root = { dataset: {} };
  const controller = createBackgroundController(root, cookies);

  assert.equal(controller.nextTransitionImage(), "/console/background/01.png");
  assert.equal(controller.nextTransitionImage(), "/console/background/02.png");
  controller.select("default");
  assert.equal(controller.nextTransitionImage(), "/console/background/default.png");
});

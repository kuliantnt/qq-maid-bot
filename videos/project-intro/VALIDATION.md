# 验证记录

- HyperFrames 0.8.34 `check --snapshots`：lint、runtime、layout、motion 均 0 错误 / 0 警告；9 个采样无布局问题；86/86 文字对比度通过。
- 已人工查看五段预览截图和从最终 MP4 提取的五段抽帧，确认子合成挂载、中文、Logo、平台限制与结尾 URL。
- 动画地图完成 53/53 tween 审计。其启发式报告包含背景圆环/面板包裹层重叠、父子时间轴重复枚举产生的 collision，以及入场/退场速度、刻意逐条显示消息的非均匀间隔提示；结合独立布局检查和成片抽帧复核，无需为这些提示改变画面。
- Studio `http://localhost:3002/#project/project-intro` 已启动，HTTP 检查返回 200。本地服务地址仅在服务运行时有效。
- `render --quality high --fps 30` 成功。ffprobe：H.264、1920×1080、30/1 fps、600 帧、20.000 秒、3,962,415 字节。
- FFmpeg 全片解码检查无错误。成片无音轨，为制作时已说明的无声版。
- `git diff --check` 通过。未修改 Rust、应用配置、部署或 Web Console，未运行 Rust/前端回归；未调用真实机器人或模型服务。

成片：`renders/qq-maid-bot-intro.mp4`（本地交付，不纳入 Git）；成片总览：`snapshots/render-contact-sheet.jpg`。

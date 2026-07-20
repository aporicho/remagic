# Remagic Manager

Remagic Manager 是 reMarkable Paper Pro Move 上的第二应用域：原版界面与镇纸仍属于“系统域”，管理器只在需要运行独立应用时接管显示。它不使用 Oxide，也不要求 KOReader 或 MagicPaper 经过镇纸/AppLoad 启动。

## 当前架构

三个项目各自负责一个边界：

- `remagic-manager`：电源键入口、显示所有权、应用生命周期、管理器 UI、QTFB 服务和故障恢复。
- `riddle-move`：MagicPaper 应用及无屏幕定时任务 agent。
- `remagic-koreader`：KOReader 的独立 QTFB 启动适配；KOReader 本体保持上游发行版。

进入管理域时，`remagicd` 先确认电源键抓取成功，再停止并验证 xochitl/Paperweight，最后启动唯一的 `remagic-runtime`。runtime 同时承载管理器 QML、应用窗口和 QTFB；应用卡片、关闭、暂停和 MagicPaper 的 `read` 请求都必须经由 `remagicd`，不会绕开状态机。

应用只有在子进程已启动、QTFB 已连接且第一帧真正进入 Qt 绘制路径后，才会从 `launching` 变成 `foreground`。暂停只隐藏窗口，进程保持运行；关闭会先请求应用保存并正常退出，KOReader 仅在 3.5 秒后仍未退出时发送 TERM、5.5 秒后才强制清理。返回系统域会先确认 runtime cgroup 已空，再恢复 xochitl/Paperweight 和电源键。

## 操作

- 原版界面三按电源键：进入管理器。
- 管理器单按电源键：召回最近使用的应用。
- 应用内单按电源键：暂停并回到管理器。
- 管理器或应用内三按电源键：返回原版/镇纸系统域。
- 管理器中的“休眠”按钮：由管理器执行系统休眠。

MagicPaper 和 KOReader 均支持手指触摸；卡片和控制按钮按下时会黑白反色。MagicPaper 仍保留笔书写，KOReader 的阅读位置由 KOReader 自身保存。

KOReader 使用官方内置的简体中文与繁体中文翻译，全新安装默认简体中文，已有语言设置保持不变。管理器还会把上图东观体的常规、粗体、细体和方正屏显雅宋安装到 KOReader 私有的 `fonts/remagic` 目录；升级时只刷新这些字体，不覆盖阅读位置和用户设置。构建机可用 `KOREADER_DONGGUAN_FONT_DIR` 与 `KOREADER_YASONG_FONT` 指定字体源文件。

## 与原始 AppLoad 的主要区别

项目固定以 `rm-appload v0.5.3` 源码归档和 SHA-256 为基线，在构建时应用仓库内的补丁。改造包括：

- 生命周期统一接入 `remagicd`，禁止卡片绕过守护进程直接启动应用。
- 单 runtime、应用单实例、独立进程组及 TERM→KILL 清理。
- `/run/remagic/runtime-app.sock` 带 request ID 的换行 JSON 控制协议。
- 可观测的 starting、QTFB connected、first frame、background、exited 状态。
- Move 原生 954×1696 RGB565、正确的脏矩形裁剪与无缩放直绘。
- 持久 QTFB 连接、可中断输入线程、共享内存/文件描述符清理和低频日志。
- 手指、压感笔和橡皮事件转发，以及管理器按钮的按下反馈。
- patched `qtfb-shim.so` 与 runtime 同源构建，不再混用官方预编译 shim。

## 构建、部署与验收

需要 reMarkable chiappa SDK、相邻的 `riddle-move`、`remagic-koreader` 和 `quill-move` 目录：

```sh
./scripts/build-bundle.sh
./scripts/deploy-usb.sh
```

设备安装默认保持原版界面为启动域，不删除书籍、KOReader 设置、MagicPaper 记忆、任务、TODO 或 API 配置。安装包在停服务之前做完整性检查；中途失败会恢复 xochitl。

部署后的自动化验收：

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/device-acceptance.sh
```

验收覆盖真实应用进程、QTFB 连接/首帧、触摸卡片、重复启动、暂停/原 PID 恢复、完整关闭、KOReader 内部退出与软键盘弹窗退出、三轮切换、显示域互斥、崩溃/core/残留资源，以及最终回到原版界面。

紧急恢复命令：

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/libexec/remagic-recover
```

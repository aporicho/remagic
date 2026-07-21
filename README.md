# Remagic Manager

Remagic Manager 是 reMarkable Paper Pro Move 上的独立应用系统层。原版界面与镇纸共同属于“系统域”；Remagic 只在运行第三方应用时接管显示、触摸和笔输入，并可随时把所有权完整还给原版系统。

它不使用 Oxide，也不把镇纸或 AppLoad 当作应用运行时。KOReader、MagicPaper 只依赖 Remagic 提供的受监督环境。

## 三个项目的边界

- `remagic-manager`：电源键入口、显示/输入唯一所有权、应用监督、前后台切换、任务页、运行环境和故障恢复。
- `riddle-move`：MagicPaper 的书写、AI、任务、TODO、历史、字体与后台 agent。
- `remagic-koreader`：上游 KOReader 的独立启动、书库、数据迁移、字体和生命周期适配。

应用不能直接抓取原始输入或物理屏幕，也不能绕过管理器切换前台。所有跨项目交互都通过带版本的 manifest、lifecycle 和 display/QTFB 契约完成。

## 系统结构

- `remagicd`：串行状态机、应用生命周期、systemd cgroup 监督和恢复策略。
- `remagic-display-host`：唯一的 Quill、面板、触摸和压感笔所有者；同时提供隔离的稳定 QTFB surface。
- `remagic-runner`：依据 schema-v2 manifest 创建每个应用的 HOME/XDG、字体、时区、证书、库、QTFB 和生命周期环境。
- `remagic-home`：手指可操作的运行中任务页，负责启动、召回、关闭和休眠入口。
- `remagicctl`：自动化、诊断和恢复使用的本机控制工具。

应用启动必须依次通过 manifest 预检、进程存活、生命周期 token、第一帧 surface 和实际面板提交。前台 lease 包含 app、generation、foreground epoch 和 lease ID；旧进程或迟到消息不能覆盖新实例。

### 应用数据 schema 契约

`[data_schema]` 只允许出现在 schema-v2 manifest。冷启动在创建 lifecycle socket、control endpoint 和应用进程之前，先完成以下事务：

1. 在应用已校验的 `state_home/.remagic-schema` 取得独占锁，先协调精确绑定 app、from/to、快照和路径集合的 `pending.json`；真正的在途事务会先恢复，同版本或降级判断都不能绕过它。
2. 对每个存在的 `backup_paths` 做不跟随符号链接的完整快照，记录文件 SHA-256、链接目标、类型、权限和属主，并在校验后原子发布。相互重叠的源路径，以及与 `.remagic-schema` 任一方向重叠的路径都会被拒绝。无 journal 的旧快照不会被误重放。
3. 若声明 `migrator`，runner 以解析完成的 schema-v2 环境、manifest `working_dir` 和空继承环境执行已经校验的同一文件描述符，不经过 shell，也不存在“校验后换文件”的路径窗口。迁移器只能是当前有效 UID 拥有、不可被 group/world 写入、无 setuid/setgid 的真实可执行文件；stdout/stderr 不进入 journal。它可读取 `REMAGIC_DATA_SCHEMA_FROM`、`REMAGIC_DATA_SCHEMA_TO` 和 `REMAGIC_DATA_SCHEMA_BACKUP`。
4. 迁移成功后原子发布 `state.json` 并退休 pending journal；无迁移器时仍先备份再记录版本。非零退出、超时、发布中断都不会更新版本，恢复后的内容、属主和权限都经过持久化同步。

runner 以 generation 绑定的 `schema-prepared`、`schema-complete` 两道原子栅栏分隔启动阶段；等待预算分别覆盖 `backup_timeout_ms`、`migration_timeout_ms + 10 秒提交余量`、应用 ready，以及 QTFB 的独立 surface/首帧时限（总上限 1450 秒），所以任何前置阶段都不会挤占后续阶段声明的时间。迁移前，管理器会先静默声明的后台写入服务，ready 后再恢复；MagicPaper 的 systemd 单元也会在重启时根据 pending journal 拒绝抢跑。新的电源键、关闭、返回或启动交互会抢占尚未完成的旧启动，客户端超时的队列事件不会迟到执行。备份与版本记录保留在应用持久 state 目录，不占用设备只读根分区。

暂停只撤销前台输入/显示 lease，应用进程和 surface 继续驻留。再次进入会恢复同一 PID 和页面。关闭先发送语义化 Shutdown，再按 manifest 的宽限期执行 TERM→KILL。返回系统域会停止全部托管 cgroup、显示宿主和共享 surface，最后恢复 xochitl 与镇纸。

## 交互

- 原版界面三按电源键：进入 Remagic 任务页。
- 应用内单按电源键：暂停应用并回到任务页。
- 任务页单按电源键：召回最近应用。
- 任务页或应用内三按电源键：返回原版/镇纸系统域。
- 任务页“休眠”：由管理器执行系统休眠。

卡片和按钮支持手指操作，按下立即反色。应用之间直接切换时不经过黑白空白页；每次进入新前台只允许一次完整刷新，普通按压、书写和返回任务页使用局部更新。

## 与早期 Remagic/AppLoad 方案的区别

- 删除 AppLoad 可执行文件、QML 启动器和第二套输入/显示所有权；只从固定、校验过的 `rm-appload v0.5.3` 源码构建 KOReader 所需的小型 `qtfb-shim.so`。
- 原来的单体 daemon/runtime 被拆为状态机、监督、运行环境、协议、显示宿主和 UI 模块。
- MagicPaper 和 KOReader 是独立 resident 应用，可暂停、直接切换、召回和完整关闭。
- QTFB surface 使用稳定 key；显示提交带 generation/epoch fence，并记录 surface hash、实际提交序号、队列深度和完整刷新次数。
- 显示宿主、Home、应用子进程或 daemon 异常退出均有明确恢复路径；无法维持托管域时优先恢复原版系统。
- 安装不覆盖书籍、KOReader 阅读位置、MagicPaper 记忆、任务、TODO、字体设置或 API 配置。
- MagicPaper 后台任务服务以持久的“应用 ID + 数据 schema 版本”栅栏启动；首装、升级、降级或断电留下迁移日志时保持停止，只有受监督的前台 runner 完成恢复/迁移后才会启动。
- KOReader 的固定程序树位于 Remagic 自有的 `remagic-koreader/program`，可写数据树与它分离；既有 `/home/root/apps/koreader` 只读作 legacy 数据源，安装和卸载都不替换或删除它。升级只迁移设置、阅读记录、截图、剪贴板和词典等数据，旧插件、用户补丁及其他可执行扩展不会自动迁移或激活。

## 构建与部署

需要 chiappa SDK，以及同级目录中的 `riddle-move`、`remagic-koreader` 和 `quill-move`：

```sh
./scripts/check.sh
./scripts/build-bundle.sh
./scripts/deploy-usb.sh
```

构建会交叉编译 aarch64 二进制，验证并打包上游 KOReader、独立 QTFB shim、四种 KOReader 中文字体，以及 MagicPaper 的手写字体和完整中文字形回退。安装前后均校验 bundle；安装失败会恢复 xochitl。

黄油拾叁体与 851 远星夜行体不从被忽略的旧 `dist` 目录取用。构建直接校验并解包原始 zip，默认位置为 `~/Downloads/黄油拾叁体.zip` 和 `~/Downloads/851远星夜行手写体.zip`；其他位置可分别用 `MAGICPAPER_BUTTER_FONT_ARCHIVE`、`MAGICPAPER_851_FONT_ARCHIVE` 指定。压缩包与解出的 TTF 都必须匹配固定 SHA-256，防止旧缓存或同名文件混入发布包。

构建还会从已校验的 KOReader v2026.03 包中复制 `NotoSansCJKsc-Regular.otf`，以 `opt/magicpaper/fonts/CoverageFallback.ttf` 纳入同一 bundle 校验和安装事务。方正屏显雅宋由适配项目校验源文件后暂存，管理器再次用固定 SHA-256 校验，并只以 `opt/magicpaper/fonts/FZPingXianYaSong.ttf` 纳入 bundle；安装器在停止任何服务前复验同一内容指纹和必需文件清单。MagicPaper 的界面统一使用方正屏显雅宋；只有回答书写和字体页的预览样例使用可切换手写体，缺少的罕见字形再逐字回退到中性完整字库。

## 自动化实机验收

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/device-acceptance-v2.sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/device-fault-acceptance-v2.sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/device-stress-acceptance-v2.sh
```

三组测试分别覆盖：

- 手指按压反馈、两应用真实首帧、墨迹收口、驻留/召回、直接切换、完整关闭和回到原版。
- Home、应用子进程、整个应用 cgroup、显示宿主和 daemon 的故障注入与自动恢复。
- 多轮 MagicPaper↔KOReader 切换、PID/surface 不变、队列排空、面板零失败、暖切换时延和每次恰好一次完整刷新。

测试不仅检查 PID，还比对生命周期 fence、目标 surface 内容签名、最后实际提交的 key/sequence 和面板提交计数。

紧急恢复：

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/libexec/remagic-recover
```

## 架构审查标准

默认生产文件以 400 行为目标、500 行为门禁，函数以 60/100 行为目标/门禁；测试文件默认上限 800 行。阈值是审查预算，不是机械拆分规则。确实更适合保持内聚的文件可在 `architecture-exceptions.tsv` 中登记精确路径、单文件上限和具体理由，禁止通配符或整目录豁免。详见 `docs/ARCHITECTURE_STANDARDS.md`。

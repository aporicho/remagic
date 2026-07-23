# ReMagic

ReMagic 是 reMarkable Paper Pro 与 Paper Pro Move 的独立应用系统层。原版界面与
镇纸共同属于“系统域”；ReMagic 只在管理或运行第三方应用时接管显示、触摸与笔
输入，并能把所有权完整还给原版系统。

它不使用 Oxide，也不把镇纸或 AppLoad 当作运行时。系统只保留从固定源码构建的
小型 QTFB 兼容 shim，KOReader、MagicPaper 均由 ReMagic 自己监督。

## 项目边界与名称

- `remagic`：系统、管理器、显示/输入唯一所有者、生命周期、安装事务与故障恢复。
- `remagic-store`：内置系统应用“应用商店”，负责签名目录、下载、校验与安装意图。
- `magicpaper`：设备上显示为 `MagicPaper`，负责书写、AI、任务、TODO、历史和字体。
- `koreader-for-remagic`：KOReader 的 ReMagic 适配工程；设备、商店和任务页一律只
  显示 `KOReader`。内部包名仍为 `koreader-for-remagic`，应用 ID 为 `koreader`。

应用不能直接取得物理屏幕或原始输入，也不能绕过管理器切换前台。跨项目交互只走
带版本的 manifest、DeviceProfile、lifecycle、display/QTFB 和 control v2 契约。

## 双设备与系统版本

安装器、daemon、显示宿主、Home、runner 和 Store 使用同一份严格 DeviceProfile：

- Paper Pro：`reMarkable Ferrari`，1620×2160，QTFB format 3。
- Paper Pro Move：`reMarkable Chiappa`，954×1696，QTFB format 6。

设备身份必须同时匹配 `/sys/devices/soc0/machine` 与
`/proc/device-tree/model`，不依据分辨率猜机型。系统兼容性读取
`/etc/os-release` 的 `IMG_VERSION`；`VERSION_ID` 只是 Codex Linux 镜像版本。
当前构建只接受正式验证的 3.27.x 系列，其他版本 fail closed。

## 系统结构

- `remagicd`：串行状态机、应用生命周期、systemd cgroup 监督和恢复。
- `remagic-display-host`：唯一 Quill、面板、触摸和压感笔所有者，并提供隔离的
  RGB565 QTFB surface。
- `remagic-runner`：依据 schema-v2 manifest 创建每个应用的 HOME/XDG、字体、
  时区、证书、网络、QTFB 和生命周期环境。
- `remagic-agentd`：按应用复用持久 Pi RPC 进程，隔离模型凭据，并为前台问答、
  预请求和定时任务提供有优先级、可取消的流式回答通道。
- `remagic-home`：手指可操作的任务页、应用商店、设置和锁屏。
- `remagic-package`：验证完整文件清单，发布不可变内容寻址 release，原子切换
  `current`，支持升级、回滚、断电恢复与保留数据卸载。
- `remagicctl`：诊断、自动化与 control v2 管理工具。

应用启动必须通过 manifest 预检、进程存活、生命周期 token、第一帧 surface 和
实际面板提交。前台 lease 绑定 app、generation、foreground epoch 和 lease ID，
旧进程与迟到消息不能覆盖新实例。

声明 `agent:pi-v1` 的应用会得到私有 socket、随机 token 与进程代际身份。交互请求会
抢占同一应用的预请求或定时请求；后台任务不能抢占正在进行的手写问答。客户端断开
会中止其活动轮次。Pi 不加载内置工具、任意扩展、技能或工程上下文；启用工具时也只
加载 ReMagic 固定的受限网页搜索扩展，应用不能借它取得 shell 或文件权限。协议与
凭据位置详见
[`docs/PI_AGENT_SERVICE.md`](docs/PI_AGENT_SERVICE.md)。

数据迁移先在应用 state 目录建立 journal 和完整快照，再以已验证的文件描述符执行
migrator；失败、超时或断电不会提交 schema。暂停先要求应用保存状态再撤销输入与
显示 lease。KOReader 后台冻结整个 cgroup，召回时恢复同一 PID、surface 和阅读页；
关闭则按语义化 Shutdown → TERM → KILL 的有界流程执行。

## 交互

- 原版界面三按电源键：进入 ReMagic。
- 应用内单按电源键：暂停应用并回到任务页。
- 任务页单按电源键：召回最近应用。
- 任务页或应用内三按电源键：返回原版/镇纸系统域。
- 任务页“休眠”：提交冻结锁屏后进入 suspend；实体电源键唤醒直接回管理器。

首次进入显示“ReMagic 已就绪”，用户可打开应用商店或返回原版系统。商店不会
自动安装用户应用；MagicPaper 与 KOReader 由用户分别选择。卡片按下立即反色，
应用切换不经过空白页，每次进入新前台只做一次必要的完整刷新。

锁屏设置可选择白纸、原生锁屏或自定义 PNG，并配置裁切、时钟和提示。设置写入
`/home/root/.config/remagic/home.toml`，壁纸位于
`/home/root/.local/share/remagic/wallpapers/`。

## 安装与更新

Linux 或 macOS 通过 USB 连接设备后运行：

```sh
curl -fsSL https://raw.githubusercontent.com/aporicho/remagic/main/install.sh | sh
```

主机脚本自动识别 Paper Pro / Paper Pro Move、验证 3.27.x、下载并校验 release，
再通过 SSH 传入设备。设备安装器会：

1. 在任何停服前复核 release 与逐文件 SHA-256。
2. 恢复原版显示所有权，再原子替换 ReMagic 系统树。
3. 事务安装不可普通卸载的 ReMagic Store。
4. 保留 `/home/root/books`、应用数据、阅读位置、API 配置、原版系统和镇纸。
5. 健康检查失败时恢复上一系统版本和已发布 manifest/unit。

重装镇纸不会修改 `/home/root/apps/remagic`；若第三方安装器重写 systemd 注册，重新
运行同一条 ReMagic 安装命令即可恢复入口，不需要重装应用或数据。

安装系统后，在电脑终端配置 MagicPaper 使用的模型密钥。脚本会在终端隐藏输入，
经 USB SSH 直接写入 ReMagic 的 root-only `0600` 供应商文件；密钥不会进入
MagicPaper 数据、应用包或设备设置界面：

```sh
bash <(curl -fsSL https://raw.githubusercontent.com/aporicho/remagic/main/configure-provider.sh) deepseek
```

将末尾的 `deepseek` 换成 `openai` 即可配置 OpenAI。脚本随后可选填写兼容服务的
API base URL；留空则使用供应商默认地址。更换密钥时重复同一条命令即可，ReMagic
会在下一次 Pi Agent 启动或手动重启后读取新配置。

## 构建

系统 release 只包含 ReMagic、内置 Store 和 KOReader 所需的小型 QTFB shim，不
包含 MagicPaper 或 KOReader 的应用 payload：

```sh
./scripts/check.sh
./scripts/build-system-release.sh
```

系统 Release 生成后会同时得到 `remagic-release-v1.json`。发布前使用独立的系统签名密钥签署：

```sh
REMAGIC_SYSTEM_SIGNING_KEY=/path/to/system-2026-01.pem \
  ./scripts/system-release/sign-release.sh dist/remagic-release-v1.json
```

设备端更新校验由 `remagic-update check|verify|apply` 完成；它只接受本仓库 GitHub
Release 路径、签名元数据和匹配的归档哈希。下载与解包使用 `/home` 中权限为
`0700` 的事务目录；成功、失败和下次启动时都会回收相应临时数据，避免占用内存盘。
应用商店继续使用独立的 Catalog 签名密钥。

默认需要当前 Paper Pro family SDK、同级 `quill-move`、`remagic-store` 和方正屏显
雅宋字体。所有 Rust/C/C++ 设备组件强制以 baseline ARMv8-A 构建，防止 Chiappa
SDK 的产品专用 `-mcpu` 指令进入 Ferrari 通用包。

应用分别在自身仓库构建独立、可复现的内容寻址包：

```sh
(cd ../magicpaper && ./scripts/check.sh && ./scripts/make-remagic-package.sh)
(cd ../koreader-for-remagic && ./scripts/check.sh && ./scripts/build-store-package.sh)
```

正式 Catalog 必须填写实际 Release URL、大小和 SHA-256，再用离线 Ed25519 私钥
签名；fixture URL 永远不能发布成可安装目录。

### CI/CD 发布

普通分支与 Pull Request 由 `.github/workflows/ci.yml` 执行完整 host 检查。正式系统
发布只需更新工作区版本与 `release/sequence`，然后推送同版本 tag（例如
`v0.1.4`）。`.github/workflows/release.yml` 会自动完成：

1. 校验并缓存官方 Paper Pro Move 3.27 SDK；
2. 固定 Store、Quill、Node、Pi 和 UI 字体的版本及 SHA-256；
3. 交叉编译并运行 ARM64 Pi 启动探针和全部发布契约；
4. 使用仓库 Secret `REMAGIC_SYSTEM_SIGNING_KEY` 签署更新元数据；
5. 将归档、校验和、签名和 release manifest 原子发布到同名 GitHub Release。

`release/sequence` 必须严格递增，它是设备拒绝降级的最终依据。SDK 从 reMarkable
官方公开工具链地址获取；UI 字体固定在 `build-assets-v1` Release，并在构建前验证
SHA-256。私钥从不进入仓库、缓存或构建产物。

## 验收与恢复

设备测试脚本安装在 `/home/root/apps/remagic/share/testing/`：

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/testing/device-acceptance-v2.sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/testing/device-fault-acceptance-v2.sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/testing/device-stress-acceptance-v2.sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/share/testing/device-lock-acceptance-v2.sh
```

它们覆盖真实首帧、按压反馈、暂停/召回/关闭、MagicPaper↔KOReader 暖切换、
surface fence、完整刷新计数、进程/cgroup/显示宿主故障恢复与 suspend/resume。
正式 stable 发布必须在 Ferrari 和 Chiappa 两台实机均通过；只有一台设备时只能发布
预发布版本。

紧急恢复：

```sh
ssh -F /dev/null root@10.11.99.1 /home/root/apps/remagic/libexec/remagic-recover
```

## 架构审查

生产文件以 400 行为目标、500 行为门禁，函数以 60/100 行为目标/门禁；测试文件
默认上限 800 行。确实更适合保持内聚的文件可在
`architecture-exceptions.tsv` 登记精确路径、上限与理由。详见
`docs/ARCHITECTURE_STANDARDS.md`。

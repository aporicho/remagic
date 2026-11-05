# ReMagic 快速迭代规范

这份规范用于缩短 ReMagic、MagicPaper、KOReader 适配和 Store 的开发反馈时间，
同时保留系统级软件必须具备的数据安全、故障恢复和双设备验收。核心原则是：
小范围失败只进入小范围循环，正式包只在问题收敛后生成一次。

## 两层循环

### 快循环：定位和收敛

快循环只使用一台代表设备，默认使用当前已连接且问题可复现的设备。

1. 记录唯一失败断言、相关日志起止时间和当前 Git commit。
2. 只运行受影响 crate、脚本或协议测试。
3. 只构建和传输受影响产物；诊断文件和测试脚本放在 `/run/remagic-dev`，不覆盖用户数据。
4. 只重跑最小复现场景，连续通过两次后退出快循环。
5. 若修复改变跨仓协议，同时运行协议两端的契约测试，但仍不立即发版。

典型主机命令：

```sh
cargo test -p remagicd daemon::server::recovery_tests
cargo test -p remagic-protocol runtime_app
cargo test -p remagic-runner lifecycle_bridge
bash tests/test-app-failure-bridge.sh
```

设备文件优先通过 USB 入口传输：

```sh
./scripts/paper-pro-move ssh 'umask 077; mkdir -p /run/remagic-dev'
./scripts/paper-pro-move push ./artifact /run/remagic-dev/artifact
./scripts/paper-pro-move ssh /run/remagic-dev/test.sh
```

不得把未验证文件写入书库、应用数据目录或内容寻址 release。需要由 systemd 调用的
临时 helper 必须通过 `/run/systemd/system` 的运行时 drop-in 指向 `/run/remagic-dev`，
验证结束后恢复原 unit；禁止直接修改 `/home/root/apps/*/current` 中的正式内容。
仓库提供 `testing/systemd/remagic-app-failed-dev.conf`。先创建对应的 runtime drop-in
目录并把它上传为 `90-remagic-dev.conf`，执行 `systemctl daemon-reload` 后测试；结束时
用 `unlink` 删除该文件并再次 `daemon-reload`。runtime drop-in 不得进入正式发布包。

```sh
./scripts/paper-pro-move ssh 'mkdir -p /run/systemd/system/remagic-app-failed@.service.d'
./scripts/paper-pro-move push testing/systemd/remagic-app-failed-dev.conf /run/systemd/system/remagic-app-failed@.service.d/90-remagic-dev.conf
./scripts/paper-pro-move ssh 'systemctl daemon-reload'
# 完成测试后：
./scripts/paper-pro-move ssh 'unlink /run/systemd/system/remagic-app-failed@.service.d/90-remagic-dev.conf'
./scripts/paper-pro-move ssh 'systemctl daemon-reload'
```

### 慢循环：合并和交付

只有快循环连续通过，才执行慢循环：

1. 运行仓库完整 `scripts/check.sh`。
2. 完成独立代码审查和 `git diff --check`。
3. 提交并等待普通分支 CI 通过。
4. 只生成一次候选 Release，并验证签名、内部 manifest、大小和 SHA-256。
5. 先部署 ReMagic，再部署要求新 API 的应用。
6. 在 Paper Pro 与 Paper Pro Move 上按分层矩阵验收。
7. 最后才进行需要拔线的真实 suspend 与功耗测试。

## 变更分级

| 等级 | 范围 | 快循环要求 | 最终要求 |
| --- | --- | --- | --- |
| L0 | 文档、注释、无执行语义 | 格式与链接检查 | 普通 CI |
| L1 | 单 crate、单 helper、单应用内部逻辑 | 定向单测＋一条最小实机场景 | 该仓全量检查 |
| L2 | 生命周期、显示、输入、包事务、跨仓协议 | 协议两端测试＋代表设备实测两次 | 双机基础/故障/交接矩阵 |
| L3 | 安装器、更新器、签名、系统恢复、功耗 | 隔离数据、回滚演练、代表设备 | 双机全矩阵＋真实拔线测试 |

不确定等级时按更高一级执行。仅因日志文字、代码行数或测试文件重排，不得无理由升级到
L2/L3；反之，任何会改变显示所有权、应用 cgroup、用户数据或签名信任链的修改都不能
降级为 L1。

## 分层实机矩阵

按以下顺序执行，任一层失败即停止后续层：

1. `device-acceptance-v2.sh`：启动、反馈、书写、暂停、召回、关闭与刷新次数。
2. `device-handoff-acceptance-v2.sh`：MagicPaper `read` 到同一 KOReader 进程。
3. `device-fault-acceptance-v2.sh`：冷启动失败、子进程/runner/display/daemon 故障。
4. `device-stress-acceptance-v2.sh`：重复切换、FD/RSS 和刷新计数。
5. `device-lock-acceptance-v2.sh`：插线锁屏与单按解锁。
6. `REMAGIC_REAL_SUSPEND=1` 和 power audit：拔线后的真实休眠、唤醒与功耗。

基础路径和交接路径优先，是因为它们最接近用户当前操作且反馈最快；故障、压力和功耗
验证负责最终交付，不应在每一次局部代码编辑后重复执行。

## 数据和发布边界

- 快循环开始前记录书库、KOReader 配置、MagicPaper 数据和原生 xochitl 数据的文件数、
  大小与文件名摘要；结束后复核。
- 隔离测试逐字节散列可变用户数据；内容寻址应用只核对 `current`、manifest 与 package
  state，完整 payload 的字节校验留给安装/发布门禁，避免每条用例重复读取数百 MB。
- 所有故障测试必须使用 `device-test-isolation.sh`，不得把测试任务、历史或阅读进度写入
  真实用户目录。
- 设备 Wi-Fi 低速时，已验证 Release 资产通过 USB 传输，再交给同一个事务化安装接口；
  不绕过 manifest、bundle 或 SHA-256 校验。
- 快循环不打 tag、不覆盖 Release 资产。Catalog revision、系统 sequence 和应用版本只在
  候选版本确定后递增一次。
- 正式发版后若实机发现新问题，先回到快循环收敛全部问题，再统一发布下一个 patch 版本。

## 完成定义

“代码能编译”不算完成。一次迭代只有同时满足以下条件才可交付：

- 原始失败场景连续两次通过；
- 受影响层的自动化测试通过且没有被放宽断言；
- 用户数据摘要未出现非预期变化；
- 两种设备的必要矩阵通过，或明确标记为尚未发布的候选版本；
- 发布资产可验证、不可用相同版本替换，并且源码仓库保持 clean。

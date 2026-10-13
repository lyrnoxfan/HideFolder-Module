# HideFolder

一个 Magisk / KernelSU / APatch 通用模块：**隐藏指定文件夹，而不是删除文件**，可随时恢复。

## 功能

- 📁 **隐藏文件夹**：用空目录 bind mount 覆盖目标文件夹，文件本身不受任何影响
- ↩️ **随时恢复**：禁用 / 删除规则立即 `umount` 恢复；卸载模块时自动恢复全部
- 🌍 **全局隐藏**：开机早期（zygote 启动前）挂载，对所有应用生效
- 📱 **按应用隐藏**：用 `setns` 进入目标应用的 mount namespace 单独挂载，其他应用不受影响
- 🖥️ **WebUI 管理**：添加 / 删除 / 启用 / 禁用规则，所见即所得
- 🔄 **后台补应用**：新启动的目标应用约 2 分钟内自动补上隐藏
- 🩺 **本机自检**：WebUI 里点「运行本机自检」，逐项检查 root / 命名空间 / 挂载能力并给出确切原因

## 架构（v2.0）

全部逻辑由单个静态 Rust 二进制 `hidefolder`（musl arm64，约几百 KB）实现，
替代了旧版的 shell 脚本。shell 只剩开机钩子 stub（Magisk 要求钩子必须是 shell
脚本）和安装脚本。直接调 `mount(2)` / `setns(2)`，不依赖 `nsenter`，
失败时返回确切的 errno 而不是模糊的"命令执行失败"。

磁盘格式与旧版完全兼容（`rules.txt` / `active.txt` / `hidefolder.log`），
升级无需迁移。

## 安装

1. 下载本仓库的 zip（根目录直接是 `module.prop` 等文件）
2. 在 Magisk / KernelSU / APatch 的模块页面选择「从本地安装」
3. 重启手机
4. 在模块的 WebUI（或 MMRL / WebUI X）中打开 HideFolder 管理规则

## 使用

### WebUI

- **添加规则**：填写要隐藏的文件夹绝对路径（如 `/sdcard/DCIM/私密相册`），选择「全局隐藏」或「仅对某个应用」并填写包名（如 `com.tencent.mm`）
- **启用 / 禁用**：点规则旁的开关，禁用立即恢复，启用立即隐藏
- **删除**：删除规则的同时恢复该文件夹
- **应用全部 / 恢复全部 / 运行本机自检**：一键操作

### 命令行（adb shell / 终端，root）

```sh
HF=/data/adb/modules/hidefolder/hidefolder
$HF list                    # 列出规则（id|启用|范围|路径）
$HF add global /sdcard/私密  # 添加全局规则
$HF add com.tencent.mm /sdcard/Download/xxx  # 添加按应用规则
$HF toggle <id>             # 启用/禁用切换
$HF enable <id> / $HF disable <id>
$HF del <id>                # 删除规则并恢复
$HF apply                   # 应用全部启用中的规则
$HF restore                 # 恢复全部隐藏
$HF status                  # 规则数 / 已启用 / 隐藏中
$HF doctor                  # 本机自检：root/ns/挂载能力逐项诊断
```

规则保存在 `/data/adb/modules/hidefolder/rules.txt`，格式为 `id|enabled|scope|path`，
手动编辑后执行 `apply` 即可生效。

## 原理

```
隐藏：mount(MS_BIND, <模块空目录>, <目标文件夹>)  → 目标看起来是空的
恢复：umount2(MNT_DETACH, <目标文件夹>)           → 文件原样回来
```

Android 有三个特殊点，模块都处理了：

1. **FUSE**：`/storage/emulated/0` 是 FUSE 挂载，真实数据在 `/data/media/0`，
   各应用看到的都是 FUSE 守护进程转交的内容。全局隐藏时会在 init 的
   mount namespace 里对下层真实路径做 bind，一处挂载、所有应用经 FUSE
   全被屏蔽（也覆盖开机前已启动的应用）。
2. **独立 mount namespace**：每个应用有自己的 mount namespace，运行时添加的
   全局规则还会逐个 namespace 补挂载；开机早期（`post-fs-data.sh`，zygote
   启动前）挂载的则会被所有应用继承。
3. **MediaStore**：系统相册走媒体库数据库而非直接读文件。隐藏时逐文件触发
   系统扫描——文件已被 bind 藏住、扫描器看不到它们，MediaStore 只会删除
   数据库中的对应行，**绝不触碰文件本身**；恢复时触发重新扫描加回。
   （注意：早期版本用过 `content delete` 清理，经验证它会连文件一起删除，
   已彻底移除，改用上述安全方式。）

按应用隐藏通过 `setns` 进入该应用的 mount namespace 执行挂载，只影响该应用。
注意：媒体库是全局共享的，按应用隐藏对走 MediaStore 读图的应用（如相册类）
无效，要彻底隐藏请用全局规则。

**失败熔断**：如果挂载一处都没成功，规则会保存但如实报告失败原因（带 errno），
不碰媒体库；重启手机后由开机脚本（真 root 环境）自动应用。所有操作幂等，
重复执行无副作用；详细日志在 `hidefolder.log`。

## 文件结构

```
HideFolder/
├── module.prop          模块元信息
├── hidefolder           arm64 静态二进制（CI 构建后提交）
├── customize.sh         安装脚本（含二进制可执行自检）
├── post-fs-data.sh      开机早期：应用全局隐藏（stub）
├── service.sh           开机后期：补应用 + 启动 Rust 守护进程（stub）
├── uninstall.sh         卸载时自动恢复全部（stub + 兜底）
├── rust/                Rust 工程（cargo）
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs      CLI / 规则 / 隐藏恢复逻辑
│       └── mnt.rs       mount(2) / setns 底层封装
├── webroot/
│   └── index.html       WebUI（清爽蓝 iOS 风格）
├── .github/workflows/
│   └── build.yml        CI：构建 arm64 静态二进制并提交回仓库
└── README.md
```

## 构建

二进制由 GitHub Actions 自动构建（`aarch64-unknown-linux-musl` 静态），
push 到 main 后自动编译并提交回仓库，无需本地交叉编译。

本地只做逻辑验证：

```sh
cd rust && cargo test        # 单元测试
cargo check --target aarch64-unknown-linux-musl  # 交叉目标类型检查
```

## 注意事项

- 隐藏的是**文件夹本身**（看起来是空目录），不会删除或修改任何文件
- 按应用隐藏按包名匹配进程 `cmdline`（`comm` 截断名兜底）
- 某些 ROM 的多开 / 应用双开进程名可能不同，规则按包名匹配主进程
- 卸载模块前建议先在 WebUI 点「恢复全部」（即使忘了，`uninstall.sh` 也会自动恢复）

## 兼容性

| 管理器 | 安装 | WebUI |
|---|---|---|
| Magisk（官方/Kitsune） | ✅ | ✅（需支持 WebUI 的版本） |
| KernelSU | ✅ | ✅ |
| APatch | ✅ | ✅ |

## License

MIT

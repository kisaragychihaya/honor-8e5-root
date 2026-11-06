# 荣耀 BKQ-AN90 一键 Root 工具包

荣耀 Magic 8（BKQ-AN90，骁龙 8 Elite Gen 5，Android 17，内核 6.12.58）**不解锁 BL** 获取 root 的一键工具。

基于 [hnubl](https://github.com/hawyn2018/honor_8e5_ubl) 的 CVE-2026-98260 提权链（AGPL-3.0）与 KernelSU 3.3.1 构建，仅限本机型本内核版本使用。

## 目录结构

```
hnubl/
├── main.bat              ← 主入口（菜单：获取 root / 禁用启用系统更新器）
├── reroot.bat            ← root 全流程（被 main.bat 调用，可单独运行）
├── reroot.sh             ← 手机端执行脚本（自动推送）
├── KernelSU_v3.3.1.apk   ← KernelSU 管理器（versionCode 32680，与资产匹配）
└── assets/
    └── magic-98260/      ← 98260 提权全套资产
        ├── run98260.sh / ko_patch / ksud / ksu-template.ko / syms
        └── 98260/        ← exploit-android-key-ptr / pc98260-su / target.sh
```

## 使用方法

1. 手机开启 USB 调试并连接电脑
2. 双击 `main.bat`
3. 菜单选择：
   - `1` 获取 root：推送资产 → 98260 内核提权 → 加载 KernelSU 模块（约 1~5 分钟）
   - `2` / `3` 禁用 / 启用系统更新器（`com.hihonor.ouc`，可恢复方式）
4. 手机上打开 KernelSU 管理器确认"工作正常"，adb shell 里 `su` 即 root

手机重启后 root 消失，重新双击 `main.bat` 选 1 即可恢复。

## 注意事项

- **临时 root**：exploit 与 KSU 模块只在内存存活，重启后需重新执行
- **内核绑定**：仅适配 `6.12.58-android16-6-g3a7ac1674240-abogki541502914-4k`；系统更新升级内核后需等待 hnubl 更新对应 profile
- 请勿清理手机 `/data/local/tmp/magic-98260/`，否则 bat 会重新推送（无害，只是多一步）
- 内核竞态利用有小概率触发设备重启，属正常现象，重跑即可
- adb 路径写在 bat 第 5 行，换环境请修改

## 免责声明

仅供学习研究，仅限操作自己的设备。基于 hnubl（AGPL-3.0）构建，相关组件许可归原项目；造成设备损坏、数据丢失或失去保修等后果自行承担。

@echo off
chcp 65001 >nul
title HNUBL 一键提权 + KernelSU
set ADB=D:\shuaji\Tools\adb.exe
set HERE=%~dp0

echo ============================================
echo  荣耀 BKQ-AN90 一键提权 + KernelSU 安装
echo  适用：重启后（或 /data/local/tmp 被清理后）
echo ============================================
echo.

echo [1/5] 等待设备连接，请插上数据线并允许 USB 调试...
%ADB% wait-for-device
%ADB% shell "echo 设备已连接: $(getprop ro.product.model)"

echo [2/6] 检查并安装 KernelSU 管理器...
%ADB% shell "pm path me.weishu.kernelsu" 2>nul | findstr "base.apk" >nul
if errorlevel 1 (
    echo      未检测到管理器，正在安装 KernelSU v3.3.1...
    %ADB% install -r "%HERE%KernelSU_v3.3.1.apk" >nul
    if errorlevel 1 (echo      管理器安装失败，请手动安装 & pause)
) else (
    echo      管理器已安装
)

echo [3/6] 推送提权资产到手机...
%ADB% shell "rm -rf /data/local/tmp/magic-98260 && mkdir -p /data/local/tmp/magic-98260/98260"
%ADB% push "%HERE%assets\magic-98260" /data/local/tmp/magic-98260 >nul
if errorlevel 1 (echo 资产推送失败！ & pause & exit /b 1)
%ADB% shell "chmod -R 755 /data/local/tmp/magic-98260"

echo [4/6] 推送 reroot 脚本...
%ADB% push "%HERE%reroot.sh" /data/local/tmp/reroot.sh >nul
%ADB% shell "chmod 755 /data/local/tmp/reroot.sh"

echo [5/6] 执行 CVE-2026-98260 提权链 + KernelSU 安装（约 1~5 分钟）...
%ADB% shell "sh /data/local/tmp/reroot.sh"

echo.
echo [6/6] 完成。请在手机上打开 KernelSU 管理器确认状态。
echo       若显示工作正常，adb shell 里 su 即可获得 root。
echo.
pause

@echo off
chcp 65001 >nul
title 荣耀 BKQ-AN90 工具主菜单
set ADB=D:\shuaji\Tools\adb.exe
set HERE=%~dp0

echo 等待设备连接...
%ADB% wait-for-device

:menu
:: 检测 root 状态
%ADB% shell "su -c id" 2>nul | findstr "uid=0" >nul
if errorlevel 1 (set ROOT_ST=未获取) else (set ROOT_ST=已获取)
:: 检测更新器状态
%ADB% shell "pm path com.hihonor.ouc" 2>nul | findstr "base.apk" >nul
if errorlevel 1 (set OUC_ST=已禁用) else (set OUC_ST=已启用)

echo.
echo ============================================
echo  荣耀 BKQ-AN90 工具主菜单
echo ============================================
echo    root 状态:  %ROOT_ST%
echo    更新器:     %OUC_ST%  (com.hihonor.ouc)
echo.
echo  1. 获取 root（98260 提权 + KernelSU 安装，约 1~5 分钟）
echo  2. 禁用系统更新器（可恢复）
echo  3. 启用系统更新器
echo  Q. 退出
echo.
set /p choice=请选择 (1/2/3/Q):
if /i "%choice%"=="1" goto root
if /i "%choice%"=="2" goto disable
if /i "%choice%"=="3" goto enable
if /i "%choice%"=="Q" goto end
echo 无效输入，请重新选择。
goto menu

:root
echo.
if "%ROOT_ST%"=="已获取" (
    set /p confirm=root 已获取，确定要重新执行提权流程? (Y/N):
    if /i not "%confirm%"=="Y" goto menu
)
call "%HERE%reroot.bat"
goto menu

:disable
echo.
echo 正在禁用系统更新器...
%ADB% shell "su -c 'pm uninstall -k --user 0 com.hihonor.ouc'"
echo.
goto menu

:enable
echo.
echo 正在启用系统更新器...
%ADB% shell "su -c 'cmd package install-existing com.hihonor.ouc'"
echo.
goto menu

:end
pause

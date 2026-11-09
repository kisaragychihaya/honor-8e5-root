@echo off
rem Build adbpair.exe and copy it next to the adb tool.
set HERE=%~dp0
pushd "%HERE%"
cargo build --release
if errorlevel 1 (popd & exit /b 1)
copy /y "target\release\adbpair.exe" "..\adbpair.exe" >nul
echo Built: %HERE%..\adbpair.exe
popd

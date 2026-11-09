@echo off
rem Build hnubl.exe and copy it next to the toolkit assets.
set HERE=%~dp0
pushd "%HERE%"
cargo build --release
if errorlevel 1 (popd & exit /b 1)
copy /y "target\release\hnubl.exe" "..\hnubl.exe" >nul
echo Built: %HERE%..\hnubl.exe
popd

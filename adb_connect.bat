@echo off
rem Shared adb connect helper, called by main.bat / reroot.bat.
rem   1) any device already in "device" state -> use it as-is
rem   2) no usable device -> try QR-code wireless pairing (adbpair.exe, if present)
rem   3) pairing skipped/failed -> ask for wireless IP:port, adb connect
rem Exports ADB_TARGET (serial) and ADB_SEL ("-s serial" or empty).
rem exit /b 1 when nothing could be connected.
rem Pure ASCII on purpose: batch parsing stays correct under any codepage.
setlocal enabledelayedexpansion

if not defined ADB (
    if exist "%~dp0adb.exe" (
        set "ADB=%~dp0adb.exe"
    ) else (
        set "ADB=D:\shuaji\Tools\adb.exe"
    )
)
set "ADB_TARGET="
set "ADB_SEL="
set "CAND="
set "NCAND=0"
set "UNAUTH="
set "WADDR="
set "GIVEUP="
set "ADBOK=1"

if not exist "%ADB%" set "ADBOK="
if not defined ADBOK echo [ERROR] adb not found: %ADB%
if not defined ADBOK echo         Check the ADB path in the script.

if defined ADBOK call :scan

if defined CAND (
    set "ADB_TARGET=!CAND!"
    set "ADB_SEL=-s !CAND!"
    rem keep legacy usb behaviour: no -s when it is the single wired device
    set "PLAIN=!CAND::=!"
    if "!PLAIN!"=="!CAND!" if "!NCAND!"=="1" set "ADB_SEL="
)

if defined ADBOK if not defined ADB_TARGET call :pairwifi
if defined ADBOK if not defined ADB_TARGET call :ask

if defined ADB_TARGET (
    echo [DEVICE] Using !ADB_TARGET!
    if defined ADB_SEL echo          Following commands will use -s !ADB_TARGET!
    endlocal & set "ADB_SEL=%ADB_SEL%" & set "ADB_TARGET=%ADB_TARGET%" & exit /b 0
)

echo.
echo No device available. Plug in the USB cable or enable wireless debugging.
echo.
pause
endlocal & set "ADB_SEL=" & set "ADB_TARGET=" & exit /b 1

rem ------------------------------------------------------------
rem  CAND = first usable device, NCAND = number of usable devices
rem ------------------------------------------------------------
:scan
for /f "tokens=1,2" %%a in ('"%ADB%" devices 2^>nul') do (
    if /i "%%b"=="device" (
        set /a NCAND+=1
        if not defined CAND set "CAND=%%a"
    )
    if /i "%%b"=="unauthorized" set "UNAUTH=1"
)
exit /b 0

rem ------------------------------------------------------------
rem  try QR-code wireless pairing via adbpair.exe (if present)
rem ------------------------------------------------------------
:pairwifi
set "PAIR_EXE=%~dp0adbpair.exe"
if not exist "%PAIR_EXE%" exit /b 0
echo.
echo  No wired device found. Trying wireless pairing...
echo  On the phone: Settings -^> Developer options -^> Wireless debugging -^> "Pair device with QR code"
echo  Then scan the QR code shown in the window with the phone camera.
set "PAIROUT="
for /f "usebackq delims=" %%L in (`"%PAIR_EXE%"`) do set "PAIROUT=%%L"
if not defined PAIROUT exit /b 0
if not "%PAIROUT:~0,11%"=="ADB_TARGET=" exit /b 0
set "PTARGET=%PAIROUT:~11%"
if not defined PTARGET (
    echo  [PAIR] Pairing cancelled or timed out.
    exit /b 0
)
echo   [OK] Paired and connected: %PTARGET%
set "ADB_TARGET=%PTARGET%"
set "ADB_SEL=-s %PTARGET%"
exit /b 0

rem ------------------------------------------------------------
rem  no usable device: ask for wireless address, try at most 3x
rem ------------------------------------------------------------
:ask
echo.
echo  No usable adb device found: no USB cable, no wireless link.
if defined UNAUTH echo  Note: a phone is attached but not authorized. Allow USB debugging on it.
echo.
echo  Pick one:
echo    1. Plug in the USB cable and allow USB debugging on the phone
echo    2. Enable Wireless debugging on the phone, then enter IP and port here
echo.
echo  Address format: 192.168.32.204:42247 or 192.168.32.204 42247
echo  Press Enter directly to give up.
for /l %%A in (1,1,3) do (
    if not defined ADB_TARGET if not defined GIVEUP call :attempt
)
if defined ADB_TARGET echo   [OK] Wireless device ready: !ADB_TARGET!
if not defined ADB_TARGET if not defined GIVEUP echo   [FAIL] Tried 3 times.
exit /b 0

rem ------------------------------------------------------------
rem  one attempt: read address -> normalize -> connect -> verify
rem ------------------------------------------------------------
:attempt
set "WADDR="
set /p "WADDR=Enter wireless debugging address [IP:port]: "
if not defined WADDR set "GIVEUP=1"
if defined GIVEUP exit /b 0

call :normalize
if not defined WADDR echo  [ERROR] Bad format. Need IP:port or IP port.
if not defined WADDR exit /b 0

echo.
echo Connecting to !WADDR! ...
set "CONNFAIL="
"%ADB%" connect !WADDR! > "%TEMP%\adb_conn_!RANDOM!.tmp" 2>&1
set "TMPCONN=%TEMP%\adb_conn_*.tmp"
for %%F in ("%TEMP%\adb_conn_*.tmp") do set "TMPCONN=%%F"
type "%TMPCONN%"
findstr /i /c:"connected to" "%TMPCONN%" >nul
if errorlevel 1 set "CONNFAIL=1"
findstr /i /c:"failed" /c:"cannot" /c:"unable" "%TMPCONN%" >nul
if not errorlevel 1 set "CONNFAIL=1"
del "%TMPCONN%" 2>nul

if defined CONNFAIL (
    echo.
    echo  [FAIL] Could not connect to !WADDR!. Check:
    echo         - Phone and PC on the same WiFi network
    echo         - Wireless debugging enabled; port may have changed
    echo         - Address typo; read the port from the phone's wireless debug page
    exit /b 0
)

set "STATE="
for /l %%i in (1,1,8) do (
    if not defined STATE (
        for /f "delims=" %%S in ('"%ADB%" -s !WADDR! get-state 2^>nul') do (
            if /i "%%S"=="device" set "STATE=1"
        )
        if not defined STATE ping -n 2 127.0.0.1 >nul
    )
)

if not defined STATE (
    echo.
    echo  [FAIL] Connected to !WADDR! but device is not ready.
    echo         If the phone asks "Allow wireless debugging?", allow it and retry.
    exit /b 0
)

set "ADB_TARGET=!WADDR!"
set "ADB_SEL=-s !WADDR!"
exit /b 0

rem ------------------------------------------------------------
rem  accept both "IP:port" and "IP port"
rem ------------------------------------------------------------
:normalize
set "RAW=!WADDR!"
set "TMP="
for /f "tokens=1,2 delims=: " %%x in ("!RAW!") do (
    if not "%%y"=="" set "TMP=%%x:%%y"
)
set "WADDR="
if not defined TMP exit /b 0
set "WADDR=!TMP!"
if not "!WADDR!"=="!RAW!" echo  Normalized to: !WADDR!
exit /b 0

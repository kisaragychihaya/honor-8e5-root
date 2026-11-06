#!/system/bin/sh
# 荣耀 BKQ-AN90 一键重新提权 + KernelSU 完整安装（重启后使用）
# 前提：/data/local/tmp/magic-98260/ 资产未被清理
set -u
D=/data/local/tmp/magic-98260
SU=$D/98260/pc98260-su
KERNEL='6.12.58-android16-6-g3a7ac1674240-abogki541502914-4k'

echo "== 1/4 执行 98260 提权链（rootd + SELinux permissive）=="
sh $D/run98260.sh || exit 1

echo "== 2/4 趁 permissive 安装 KSU userspace 组件（su 等）=="
printf '%s install; echo INSTALL_RC=$?\n' "$D/ksud" | $SU 2>/dev/null | grep -E 'INSTALL_RC|install'

echo "== 3/4 修补并加载 KernelSU 内核模块 =="
SLIDE=$(sed -n 's/.*"slide":"\(0x[0-9a-fA-F]*\)".*/\1/p' $D/98260-check.json | tail -1)
[ -n "$SLIDE" ] || { echo "SLIDE 获取失败"; exit 1; }
$D/ko_patch $D/ksu-template.ko $D/98260/ksu-patched.ko $D/syms_61258.txt "$SLIDE" "$KERNEL" \
    | grep -E 'PATCHED=|VERMAGIC=' || { echo "ko_patch 失败"; exit 1; }

printf 'mkdir -p /data/adb/ksu/lib && cp %s /data/adb/ksu/lib/android16-6.12_kernelsu.ko && chmod 644 /data/adb/ksu/lib/android16-6.12_kernelsu.ko\n%s late-load --kmi android16-6.12 --package-name me.weishu.kernelsu --allow-shell\n' \
    "$D/98260/ksu-patched.ko" "$D/ksud" | $SU >/dev/null 2>&1

echo "== 4/4 验证 KSU 通道 =="
sleep 2
if grep -q '^kernelsu ' /proc/modules; then
    echo "KSU_MODULE_OK"
else
    printf 'insmod %s\n' "$D/98260/ksu-patched.ko" | $SU >/dev/null 2>&1
    sleep 1
    grep -q '^kernelsu ' /proc/modules && echo "KSU_MODULE_OK(insmod)" || echo "KSU_MODULE_FAIL"
fi

# late-load 后 SELinux 恢复 enforcing，rootd 通道失效属预期，不再使用 pc98260-su
echo "== 验证 su（可能需要先在管理器里授权）=="
/data/adb/ksu/bin/su -c 'id' 2>/dev/null && echo "KSU_SU_OK" || echo "su 未就绪：打开 KernelSU 管理器确认状态"
echo "完成"

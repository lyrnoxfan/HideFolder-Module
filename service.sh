#!/system/bin/sh
# late_start：在 /sdcard 就绪后补应用全局规则，
# 并启动 Rust 守护进程，定期为新启动的目标应用补按应用隐藏规则
MODDIR=${0%/*}
"$MODDIR/hidefolder" apply global-only
"$MODDIR/hidefolder" daemon &

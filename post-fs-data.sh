#!/system/bin/sh
# 开机早期（zygote 启动前）应用全局隐藏规则，
# 此时挂载对之后启动的所有应用都可见
MODDIR=${0%/*}
"$MODDIR/hidefolder" boot-early

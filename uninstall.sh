#!/system/bin/sh
# 卸载模块时自动恢复所有隐藏的文件夹
MODDIR=${0%/*}

if [ -x "$MODDIR/hidefolder" ]; then
  "$MODDIR/hidefolder" restore
else
  # 兜底：二进制不在时，按记录逐个懒卸载
  if [ -f "$MODDIR/active.txt" ]; then
    while IFS='|' read -r kind a b; do
      case "$kind" in
        global) umount -l "$a" 2>/dev/null ;;
        app) umount -l "$b" 2>/dev/null ;;
      esac
    done < "$MODDIR/active.txt"
  fi
  if [ -f "$MODDIR/rules.txt" ]; then
    while IFS='|' read -r id enabled scope path rest; do
      case "$id" in ''|\#*) continue ;; esac
      umount -l "$path" 2>/dev/null
    done < "$MODDIR/rules.txt"
  fi
fi

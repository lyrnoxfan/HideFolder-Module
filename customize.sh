# HideFolder 安装脚本（兼容 Magisk / KernelSU / APatch）

ui_print "- 正在安装 HideFolder ..."
mkdir -p "$MODPATH/empty" "$MODPATH/webroot"
# 二进制与开机脚本必须可执行：Magisk / KernelSU / APatch 都是直接 exec 执行，
# zip 包里的 644 权限会导致开机挂载逻辑完全不运行
chmod 755 "$MODPATH/hidefolder"
chmod 755 "$MODPATH"/post-fs-data.sh "$MODPATH"/service.sh "$MODPATH"/uninstall.sh
chmod 644 "$MODPATH/module.prop"
: > "$MODPATH/rules.txt"
: > "$MODPATH/active.txt"
if "$MODPATH/hidefolder" version >/dev/null 2>&1; then
  ui_print "- 二进制自检通过：$("$MODPATH/hidefolder" version)"
else
  ui_print "! 警告：hidefolder 二进制无法执行（可能是架构不匹配），模块将无法工作"
fi
ui_print "- 安装完成"
ui_print "- 原理：bind mount 空目录覆盖目标文件夹，文件本身不受影响"
ui_print "- 在 Magisk / KernelSU / APatch 的 WebUI 中管理隐藏规则"

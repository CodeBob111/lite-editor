#!/bin/sh
# 重打 vendored 依赖补丁。
#
# 背景:gpui-component 是 git 依赖,cargo 按 git rev 给 checkout 做指纹、当只读缓存 ——
# 直接改 checkout 不会被采用(check 秒过、不重编),需 clean 后重编;且任何刷新 checkout 的
# 操作(cargo gc、升级 Cargo.lock 里 gpui-component 的 rev、换机器重新 fetch)都会丢掉补丁。
# 行为回退(双击行尾又选下一行 / 文件内搜索不居中)时,八成是补丁没了 —— 跑本脚本重打。
#
# 补丁(patches/*.patch 全量应用,幂等):
#   gpui-component-select_word.patch
#     select_word 行尾双击修正:双击落在行末时,word_range 取到的"词"是行尾换行符,选中它会让
#     光标落到下一行、整行高亮(非 IDEA 行为,且因在 mouse-down 选中、靠 mouse-up 消不掉而闪烁)。
#     补丁改成:换行符不当词 —— 从其左侧一格取词,选中本行最后一个 token(与 IDEA 双击行尾一致),
#     行首/空行则收起光标不选。结构上不再牵连下一行、无闪。
#   gpui-component-search-center.patch
#     文件内搜索(Cmd+F)next/prev 定位修正:原实现只 scroll_to、不移动光标,光标停在搜索前的旧行
#     (当前行高亮指向错误位置、匹配淡且不居中)。补丁改成选中当前匹配 —— 光标落到匹配、选区高亮
#     当前匹配,且 selected_range 变化触发 layout_cursor 在 surrounding_lines=9999(Nib 搜索时开)
#     下把匹配滚到视口居中。
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)

# Cargo.lock 锁定 gpui-component 的完整 rev;checkout 子目录用其短 rev(前 7 位)
FULLREV=$(grep -A2 '^name = "gpui-component"$' "$ROOT/Cargo.lock" \
  | grep -oE 'gpui-component#[0-9a-f]{40}' | grep -oE '[0-9a-f]{40}' | head -1)
[ -n "$FULLREV" ] || { echo "找不到 gpui-component 的 rev(Cargo.lock)"; exit 1; }
SHORT=$(printf '%s' "$FULLREV" | cut -c1-7)

# 用任一已知文件定位 checkout 根
TARGET=$(find "$HOME/.cargo/git/checkouts" \
  -path "*gpui-component*/$SHORT/crates/ui/src/input/selection.rs" 2>/dev/null | head -1)
[ -n "$TARGET" ] || { echo "找不到 gpui-component checkout($SHORT);先 cargo fetch"; exit 1; }
CKROOT=${TARGET%/crates/ui/src/input/selection.rs}

# 幂等应用全部补丁:能反向应用 = 已打过,跳过;否则正向打。两者都不行 = 冲突,报错。
applied=0
for PATCH in "$ROOT"/patches/*.patch; do
  name=$(basename "$PATCH")
  if git -C "$CKROOT" apply --reverse --check "$PATCH" 2>/dev/null; then
    echo "已应用,跳过:$name"
  elif git -C "$CKROOT" apply --check "$PATCH" 2>/dev/null; then
    git -C "$CKROOT" apply "$PATCH"
    echo "已打补丁:$name"
    applied=1
  else
    echo "无法应用(冲突?rev 变了?):$name"; exit 1
  fi
done

if [ "$applied" -eq 0 ]; then
  echo "所有补丁已在 checkout($SHORT),无需重编"
  exit 0
fi

# git 源按 rev 指纹、不按 mtime 重编;clean 掉 gpui-component 强制下次构建带补丁重编。
# 注意:`cargo clean -p` 不带 --release 只清 dev profile —— release(装机用的 profile)要单独清,
# 否则 release 构建会链到 patch 前的旧 .rlib(实测会静默用旧产物,3s "Finished" 但不含补丁)。
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo clean -p gpui-component --manifest-path "$ROOT/Cargo.toml"
cargo clean -p gpui-component --release --manifest-path "$ROOT/Cargo.toml"
echo "已 clean gpui-component(dev+release),下次构建将带补丁重编"

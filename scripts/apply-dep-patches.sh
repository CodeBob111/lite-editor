#!/bin/sh
# 重打 vendored 依赖补丁。
#
# 背景:gpui-component 是 git 依赖,cargo 按 git rev 给 checkout 做指纹、当只读缓存 ——
# 直接改 checkout 不会被采用(check 秒过、不重编),需 clean 后重编;且任何刷新 checkout 的
# 操作(cargo gc、升级 Cargo.lock 里 gpui-component 的 rev、换机器重新 fetch)都会丢掉补丁。
# 出现「双击行尾又选中下一行」时,八成是补丁没了 —— 跑本脚本重打。
#
# 补丁:patches/gpui-component-select_word.patch
#   select_word 行尾双击修正:双击落在行末时,word_range 取到的"词"是行尾换行符,选中它会让
#   光标落到下一行、整行高亮(非 IDEA 行为,且因在 mouse-down 选中、靠 mouse-up 消不掉而闪烁)。
#   补丁改成:换行符不当词 —— 从其左侧一格取词,选中本行最后一个 token(与 IDEA 双击行尾一致),
#   行首/空行则收起光标不选。结构上不再牵连下一行、无闪。
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PATCH="$ROOT/patches/gpui-component-select_word.patch"

# Cargo.lock 锁定 gpui-component 的完整 rev;checkout 子目录用其短 rev(前 7 位)
FULLREV=$(grep -A2 '^name = "gpui-component"$' "$ROOT/Cargo.lock" \
  | grep -oE 'gpui-component#[0-9a-f]{40}' | grep -oE '[0-9a-f]{40}' | head -1)
[ -n "$FULLREV" ] || { echo "找不到 gpui-component 的 rev(Cargo.lock)"; exit 1; }
SHORT=$(printf '%s' "$FULLREV" | cut -c1-7)

TARGET=$(find "$HOME/.cargo/git/checkouts" \
  -path "*gpui-component*/$SHORT/crates/ui/src/input/selection.rs" 2>/dev/null | head -1)
[ -n "$TARGET" ] || { echo "找不到 gpui-component checkout($SHORT);先 cargo fetch"; exit 1; }
CKROOT=${TARGET%/crates/ui/src/input/selection.rs}

if grep -q '\[nib patch\]' "$TARGET"; then
  echo "补丁已在 checkout($SHORT),跳过 apply"
else
  git -C "$CKROOT" apply "$PATCH"
  echo "已打补丁 → $CKROOT"
fi

# git 源按 rev 指纹、不按 mtime 重编;clean 掉 gpui-component 强制下次构建带补丁重编。
# 注意:`cargo clean -p` 不带 --release 只清 dev profile —— release(装机用的 profile)要单独清,
# 否则 release 构建会链到 patch 前的旧 .rlib(实测会静默用旧产物,3s "Finished" 但不含补丁)。
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo clean -p gpui-component --manifest-path "$ROOT/Cargo.toml"
cargo clean -p gpui-component --release --manifest-path "$ROOT/Cargo.toml"
echo "已 clean gpui-component(dev+release),下次构建将带补丁重编"

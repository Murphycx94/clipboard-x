export const isMac =
  navigator.platform.toUpperCase().includes("MAC") ||
  navigator.userAgent.includes("Mac");

/** 生成快捷键文案，index 为 1~9 */
export function getShortcutLabel(index: number): string {
  return isMac ? `⌘⇧${index}` : `Ctrl+Shift+${index}`;
}

/** 判断 keydown 事件是否匹配快捷键 */
export function isShortcutKey(e: KeyboardEvent): number | null {
  const digit = parseInt(e.key);
  if (digit >= 1 && digit <= 9) {
    if (isMac ? e.metaKey && e.shiftKey : e.ctrlKey && e.shiftKey) {
      return digit;
    }
  }
  return null;
}

import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { Spin, Empty } from "@arco-design/web-react";
import { getFavoriteItems, copyToClipboard, hideWindow } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";
import { useClipboardStore } from "../store/useClipboardStore";
import { isShortcutKey } from "../utils/platform";

export function FavoriteList() {
  const showImageOnly = useClipboardStore((s) => s.showImageOnly);
  const { data: items = [], isLoading } = useQuery({
    queryKey: ["favorite-items"],
    queryFn: getFavoriteItems,
    refetchInterval: 2000,
  });

  const filtered = showImageOnly ? items.filter((item) => item.content_type === "image") : items;

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const shortcutDigit = isShortcutKey(e);
      if (shortcutDigit !== null) {
        e.preventDefault();
        const target = filtered[shortcutDigit - 1];
        if (target) {
          copyToClipboard(target.id).then(() => hideWindow());
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [filtered]);

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Spin size={20} />
      </div>
    );
  }

  if (filtered.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Empty description="暂无收藏，点击条目右侧 ★ 添加" />
      </div>
    );
  }

  return (
    <div className="overflow-y-auto flex-1">
      {filtered.map((item, index) => (
        <ClipboardItemCard
          key={item.id}
          item={item}
          shortcutIndex={index < 9 ? index + 1 : undefined}
        />
      ))}
    </div>
  );
}

import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { Spin, Empty } from "@arco-design/web-react";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { getTodayItems, copyToClipboard } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";
import { useClipboardStore } from "../store/useClipboardStore";

const hideWindow = () => getCurrentWindow().setPosition(new PhysicalPosition(-10000, -10000));

export function HistoryList() {
  const searchQuery = useClipboardStore((s) => s.searchQuery);
  const showImageOnly = useClipboardStore((s) => s.showImageOnly);
  const focusedIndex = useClipboardStore((s) => s.focusedIndex);
  const setFocusedIndex = useClipboardStore((s) => s.setFocusedIndex);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);

  const { data: items = [], isLoading } = useQuery({
    queryKey: ["today-items"],
    queryFn: getTodayItems,
    refetchInterval: 1000,
  });

  const filtered = items.filter((item) => {
    if (showImageOnly && item.content_type !== "image") return false;
    if (!searchQuery) return true;
    return item.text_content?.toLowerCase().includes(searchQuery.toLowerCase());
  });

  // 焦点项滚动到可见区域
  useEffect(() => {
    itemRefs.current[focusedIndex]?.scrollIntoView({ block: "nearest" });
  }, [focusedIndex]);

  // 键盘导航
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (filtered.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setFocusedIndex(Math.min(focusedIndex + 1, filtered.length - 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setFocusedIndex(Math.max(focusedIndex - 1, 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        copyToClipboard(filtered[focusedIndex].id).then(() => {
          hideWindow();
        });
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [filtered, focusedIndex, setFocusedIndex]);

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
        <Empty description={searchQuery ? "无匹配内容" : "今日暂无复制记录"} />
      </div>
    );
  }

  return (
    <div className="overflow-y-auto flex-1">
      {filtered.map((item, index) => (
        <div key={item.id} ref={(el) => { itemRefs.current[index] = el; }}>
          <ClipboardItemCard
            item={item}
            focused={index === focusedIndex}
            onHover={() => setFocusedIndex(index)}
          />
        </div>
      ))}
    </div>
  );
}

import { useQuery } from "@tanstack/react-query";
import { Spin, Empty } from "@arco-design/web-react";
import { getTodayItems } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";
import { useClipboardStore } from "../store/useClipboardStore";

export function HistoryList() {
  const searchQuery = useClipboardStore((s) => s.searchQuery);
  const { data: items = [], isLoading } = useQuery({
    queryKey: ["today-items"],
    queryFn: getTodayItems,
    refetchInterval: 1000,
  });

  const filtered = items.filter((item) => {
    if (!searchQuery) return true;
    return item.text_content?.toLowerCase().includes(searchQuery.toLowerCase());
  });

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
      {filtered.map((item) => (
        <ClipboardItemCard key={item.id} item={item} />
      ))}
    </div>
  );
}

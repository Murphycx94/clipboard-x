import { useQuery } from "@tanstack/react-query";
import { getArchiveItems } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";

export function ArchivePage() {
  const { data: groups = [], isLoading } = useQuery({
    queryKey: ["archive-items"],
    queryFn: getArchiveItems,
  });
  const [expandedDates, setExpandedDates] = useState<Set<string>>(new Set());

  const toggleDate = (date: string) => {
    setExpandedDates((prev) => {
      const next = new Set(prev);
      next.has(date) ? next.delete(date) : next.add(date);
      return next;
    });
  };

  if (isLoading) return <div className="p-4 text-sm text-gray-400">加载中...</div>;

  if (groups.length === 0) {
    return (
      <div className="p-8 text-center text-sm text-gray-400">暂无历史记录</div>
    );
  }

  return (
    <div className="overflow-y-auto flex-1">
      {groups.map((group) => {
        const expanded = expandedDates.has(group.date);
        return (
          <div key={group.date}>
            <button
              onClick={() => toggleDate(group.date)}
              className="w-full flex items-center gap-2 px-4 py-2 bg-gray-50 hover:bg-gray-100 text-sm font-medium text-gray-600 border-b border-gray-200"
            >
              {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
              {group.date}
              <span className="text-xs text-gray-400 ml-auto">{group.items.length} 条</span>
            </button>
            {expanded && group.items.map((item) => (
              <ClipboardItemCard key={item.id} item={item} />
            ))}
          </div>
        );
      })}
    </div>
  );
}

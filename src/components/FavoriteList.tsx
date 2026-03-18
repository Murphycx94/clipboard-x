import { useQuery } from "@tanstack/react-query";
import { getFavoriteItems } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";

export function FavoriteList() {
  const { data: items = [], isLoading } = useQuery({
    queryKey: ["favorite-items"],
    queryFn: getFavoriteItems,
    refetchInterval: 2000,
  });

  if (isLoading) {
    return <div className="p-4 text-sm text-gray-400">加载中...</div>;
  }

  if (items.length === 0) {
    return (
      <div className="p-8 text-center text-sm text-gray-400">
        暂无收藏内容，点击历史记录中的 ★ 按钮添加收藏
      </div>
    );
  }

  return (
    <div className="overflow-y-auto flex-1">
      {items.map((item) => (
        <ClipboardItemCard key={item.id} item={item} />
      ))}
    </div>
  );
}

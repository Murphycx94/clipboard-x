import { useQuery } from "@tanstack/react-query";
import { Spin, Empty } from "@arco-design/web-react";
import { getFavoriteItems } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";

export function FavoriteList() {
  const { data: items = [], isLoading } = useQuery({
    queryKey: ["favorite-items"],
    queryFn: getFavoriteItems,
    refetchInterval: 2000,
  });

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Spin size={20} />
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Empty description="暂无收藏，点击条目右侧 ★ 添加" />
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

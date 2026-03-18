import { useQuery } from "@tanstack/react-query";
import { Spin, Empty, Collapse } from "@arco-design/web-react";
import { getArchiveItems } from "../api/commands";
import { ClipboardItemCard } from "./ClipboardItemCard";

const CollapseItem = Collapse.Item;

export function ArchivePage() {
  const { data: groups = [], isLoading } = useQuery({
    queryKey: ["archive-items"],
    queryFn: getArchiveItems,
  });

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Spin size={20} />
      </div>
    );
  }

  if (groups.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <Empty description="暂无历史记录" />
      </div>
    );
  }

  return (
    <div className="overflow-y-auto flex-1">
      <Collapse bordered={false} style={{ background: "transparent" }}>
        {groups.map((group) => (
          <CollapseItem
            key={group.date}
            header={
              <span className="text-sm font-medium text-gray-600 dark:text-gray-300">
                {group.date}
                <span className="ml-2 text-xs text-gray-400 dark:text-gray-500">{group.items.length} 条</span>
              </span>
            }
            name={group.date}
            style={{ borderBottom: "1px solid rgb(243 244 246 / 0.1)" }}
          >
            {group.items.map((item) => (
              <ClipboardItemCard key={item.id} item={item} />
            ))}
          </CollapseItem>
        ))}
      </Collapse>
    </div>
  );
}

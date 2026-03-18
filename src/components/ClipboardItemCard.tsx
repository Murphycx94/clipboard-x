import { ClipboardItem } from "../types";
import { Copy, Star, Trash2 } from "lucide-react";
import { copyToClipboard, toggleFavorite, deleteItem } from "../api/commands";
import { useQueryClient } from "@tanstack/react-query";

interface Props {
  item: ClipboardItem;
}

export function ClipboardItemCard({ item }: Props) {
  const queryClient = useQueryClient();

  const handleCopy = async () => {
    await copyToClipboard(item.id);
  };

  const handleFavorite = async () => {
    const note = item.is_favorite ? "" : (item.note ?? "");
    await toggleFavorite(item.id, note);
    queryClient.invalidateQueries();
  };

  const handleDelete = async () => {
    await deleteItem(item.id);
    queryClient.invalidateQueries();
  };

  const timeStr = new Date(item.created_at).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  });

  return (
    <div className="group flex items-start gap-3 px-4 py-3 hover:bg-gray-50 rounded-lg transition-colors">
      <div className="text-xs text-gray-400 w-10 mt-0.5 shrink-0">{timeStr}</div>

      <div className="flex-1 min-w-0">
        {item.content_type === "text" ? (
          <p className="text-sm text-gray-800 truncate">{item.text_content}</p>
        ) : (
          <div className="flex items-center gap-2">
            {item.image_thumbnail && (
              <img
                src={`data:image/png;base64,${item.image_thumbnail}`}
                alt="clipboard image"
                className="h-10 w-16 object-cover rounded border border-gray-200"
              />
            )}
            <span className="text-xs text-gray-400">[图片]</span>
          </div>
        )}
        {item.note && (
          <p className="text-xs text-indigo-500 mt-0.5 truncate">备注: {item.note}</p>
        )}
      </div>

      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
        <button
          onClick={handleCopy}
          className="p-1.5 rounded hover:bg-gray-200 text-gray-500 hover:text-gray-700"
          title="复制"
        >
          <Copy size={14} />
        </button>
        <button
          onClick={handleFavorite}
          className={`p-1.5 rounded hover:bg-gray-200 ${item.is_favorite ? "text-yellow-500" : "text-gray-400 hover:text-yellow-500"}`}
          title={item.is_favorite ? "取消收藏" : "收藏"}
        >
          <Star size={14} fill={item.is_favorite ? "currentColor" : "none"} />
        </button>
        <button
          onClick={handleDelete}
          className="p-1.5 rounded hover:bg-gray-200 text-gray-400 hover:text-red-500"
          title="删除"
        >
          <Trash2 size={14} />
        </button>
      </div>
    </div>
  );
}

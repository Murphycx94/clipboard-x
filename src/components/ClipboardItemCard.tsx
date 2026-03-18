import { useState } from "react";
import { Button } from "@arco-design/web-react";
import { IconStar, IconStarFill, IconDelete } from "@arco-design/web-react/icon";
import { Check } from "lucide-react";
import { ClipboardItem } from "../types";
import { copyToClipboard, toggleFavorite, deleteItem } from "../api/commands";
import { useQueryClient } from "@tanstack/react-query";

interface Props {
  item: ClipboardItem;
}

export function ClipboardItemCard({ item }: Props) {
  const queryClient = useQueryClient();
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    await copyToClipboard(item.id);
    setCopied(true);
    setTimeout(() => setCopied(false), 1000);
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
    <div
      onClick={handleCopy}
      className={`group flex items-center gap-3 px-4 py-3 transition-colors cursor-pointer ${
        copied ? "bg-indigo-50" : "hover:bg-gray-50"
      }`}
    >
      <span className="text-xs text-gray-400 w-10 shrink-0">{timeStr}</span>

      <div className="flex-1 min-w-0">
        {item.content_type === "text" ? (
          <p className="text-sm text-gray-800 truncate" title={item.text_content ?? ""}>
            {item.text_content}
          </p>
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
          <p className="text-xs text-indigo-500 mt-0.5 truncate">备注：{item.note}</p>
        )}
      </div>

      <div className="flex items-center gap-0.5 shrink-0">
        {copied ? (
          <div className="flex items-center gap-0.5">
            <Button type="text" size="mini" icon={<Check size={16} />} style={{ color: "rgb(99 102 241)" }} />
            <Button type="text" size="mini" icon={<IconStar style={{ fontSize: 16 }} />} style={{ opacity: 0 }} />
            <Button type="text" size="mini" icon={<IconDelete style={{ fontSize: 16 }} />} style={{ opacity: 0 }} />
          </div>
        ) : (
          <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
            <div onClick={(e) => e.stopPropagation()}>
              <Button
                type="text"
                size="mini"
                icon={item.is_favorite ? <IconStarFill style={{ fontSize: 16 }} /> : <IconStar style={{ fontSize: 16 }} />}
                onClick={handleFavorite}
                style={{ color: item.is_favorite ? "rgb(234 179 8)" : "rgb(156 163 175)" }}
              />
            </div>
            <div onClick={(e) => e.stopPropagation()}>
              <Button
                type="text"
                size="mini"
                icon={<IconDelete style={{ fontSize: 16 }} />}
                onClick={handleDelete}
                style={{ color: "rgb(156 163 175)" }}
                className="hover:!text-red-500"
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

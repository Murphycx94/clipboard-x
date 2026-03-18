import { useState } from "react";
import { Search, Archive, Settings, X } from "lucide-react";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";
import { useClipboardStore } from "../store/useClipboardStore";
import { HistoryList } from "./HistoryList";
import { FavoriteList } from "./FavoriteList";
import { ArchivePage } from "./ArchivePage";
import { SettingsPanel } from "./SettingsPanel";

const hideWindow = () => getCurrentWindow().setPosition(new PhysicalPosition(-10000, -10000));

type View = "main" | "archive" | "settings";

export function MainPanel() {
  const { activeTab, setActiveTab, searchQuery, setSearchQuery } = useClipboardStore();
  const [view, setView] = useState<View>("main");

  return (
    <div className="flex flex-col flex-1 bg-white rounded-xl overflow-hidden">
      {/* Header — data-tauri-drag-region 使整行可拖动 */}
      <div data-tauri-drag-region className="flex items-center justify-between px-4 py-3 border-b border-gray-100 select-none">
        <span className="font-semibold text-gray-800 text-sm">ClipboardX</span>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setView(view === "archive" ? "main" : "archive")}
            className={`p-1.5 rounded hover:bg-gray-100 ${view === "archive" ? "text-indigo-500" : "text-gray-500"}`}
            title="历史归档"
          >
            <Archive size={15} />
          </button>
          <button
            onClick={() => setView(view === "settings" ? "main" : "settings")}
            className={`p-1.5 rounded hover:bg-gray-100 ${view === "settings" ? "text-indigo-500" : "text-gray-500"}`}
            title="设置"
          >
            <Settings size={15} />
          </button>
          <button
            onClick={hideWindow}
            className="p-1.5 rounded hover:bg-gray-100 text-gray-400 hover:text-gray-600"
            title="关闭"
          >
            <X size={15} />
          </button>
        </div>
      </div>

      {view === "archive" ? (
        <ArchivePage />
      ) : view === "settings" ? (
        <SettingsPanel />
      ) : (
        <>
          {/* Tabs */}
          <div className="flex border-b border-gray-100">
            {(["history", "favorites"] as const).map((tab) => (
              <button
                key={tab}
                onClick={() => setActiveTab(tab)}
                className={`flex-1 py-2 text-sm font-medium transition-colors ${
                  activeTab === tab
                    ? "text-indigo-600 border-b-2 border-indigo-500"
                    : "text-gray-500 hover:text-gray-700"
                }`}
              >
                {tab === "history" ? "今日历史" : "收藏夹"}
              </button>
            ))}
          </div>

          {/* Search */}
          <div className="px-3 py-2 border-b border-gray-100">
            <div className="flex items-center gap-2 bg-gray-50 rounded-lg px-3 py-1.5">
              <Search size={13} className="text-gray-400" />
              <input
                type="text"
                placeholder="搜索..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="flex-1 bg-transparent text-sm outline-none text-gray-700 placeholder-gray-400"
              />
            </div>
          </div>

          {activeTab === "history" ? <HistoryList /> : <FavoriteList />}
        </>
      )}
    </div>
  );
}

import { useState, useEffect } from "react";
import { Radio, Input, Button } from "@arco-design/web-react";
import { IconArchive, IconSettings, IconClose, IconSearch } from "@arco-design/web-react/icon";
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

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    win.onFocusChanged(({ payload: focused }) => {
      if (!focused) hideWindow();
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  return (
    <div className="flex flex-col flex-1 bg-white rounded-xl overflow-hidden">
      {/* Header */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-4 py-2 border-b border-gray-100 select-none"
        style={{ minHeight: 44 }}
      >
        <span className="font-semibold text-gray-800 text-sm">ClipboardX</span>
        <div className="flex items-center gap-0.5">
          <Button
            type="text"
            size="mini"
            icon={<IconArchive style={{ fontSize: 16 }} />}
            onClick={() => setView(view === "archive" ? "main" : "archive")}
            style={{ color: view === "archive" ? "rgb(99 102 241)" : "rgb(107 114 128)" }}
          />
          <Button
            type="text"
            size="mini"
            icon={<IconSettings style={{ fontSize: 16 }} />}
            onClick={() => setView(view === "settings" ? "main" : "settings")}
            style={{ color: view === "settings" ? "rgb(99 102 241)" : "rgb(107 114 128)" }}
          />
          <Button
            type="text"
            size="mini"
            icon={<IconClose style={{ fontSize: 16 }} />}
            onClick={hideWindow}
            style={{ color: "rgb(156 163 175)" }}
          />
        </div>
      </div>

      {view === "archive" ? (
        <ArchivePage />
      ) : view === "settings" ? (
        <SettingsPanel />
      ) : (
        <div className="flex flex-col flex-1 min-h-0">
          <div className="px-4 py-2 border-b border-gray-100">
            <Radio.Group
              type="button"
              value={activeTab}
              onChange={(val) => setActiveTab(val as "history" | "favorites")}
              size="small"
            >
              <Radio value="history">今日历史</Radio>
              <Radio value="favorites">收藏夹</Radio>
            </Radio.Group>
          </div>

          <div className="px-3 py-2 border-b border-gray-100">
            <Input
              size="small"
              placeholder="搜索..."
              prefix={<IconSearch style={{ color: "rgb(156 163 175)", fontSize: 16 }} />}
              value={searchQuery}
              onChange={setSearchQuery}
              style={{ background: "rgb(249 250 251)", border: "none", borderRadius: 8 }}
            />
          </div>

          {activeTab === "history" ? <HistoryList /> : <FavoriteList />}
        </div>
      )}
    </div>
  );
}

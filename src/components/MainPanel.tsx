import { useState, useEffect, useRef } from "react";
import { Radio, Input } from "@arco-design/web-react";
import { IconSearch, IconClose, IconImage } from "@arco-design/web-react/icon";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useClipboardStore } from "../store/useClipboardStore";
import { HistoryList } from "./HistoryList";
import { FavoriteList } from "./FavoriteList";
import { ArchivePage } from "./ArchivePage";
import { SettingsPanel, getHideOnBlur } from "./SettingsPanel";
import { PanelHeader } from "./PanelHeader";
import { hideWindow } from "../api/commands";

type View = "main" | "archive" | "settings";

export function MainPanel() {
  const { activeTab, setActiveTab, searchQuery, setSearchQuery, setFocusedIndex, showImageOnly, toggleShowImageOnly } = useClipboardStore();
  const [view, setView] = useState<View>("main");
  const inputRef = useRef<{ focus: () => void }>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "ArrowLeft") { setActiveTab("history"); return; }
      if (e.key === "ArrowRight") { setActiveTab("favorites"); return; }
      if (e.key === "Tab") {
        e.preventDefault();
        setActiveTab(activeTab === "history" ? "favorites" : "history");
        return;
      }
      if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
        inputRef.current?.focus();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [setActiveTab, activeTab]);

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    win.onFocusChanged(({ payload: focused }) => {
      if (!focused) {
        if (getHideOnBlur()) hideWindow();
      } else {
        setFocusedIndex(0);
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  return (
    <div className="flex flex-col flex-1 bg-white dark:bg-[#1d1d1f] rounded-xl overflow-hidden">
      <PanelHeader view={view} onViewChange={setView} />

      {view === "archive" ? (
        <ArchivePage />
      ) : view === "settings" ? (
        <SettingsPanel />
      ) : (
        <div className="flex flex-col flex-1 min-h-0">
          <div className="flex items-center gap-4 px-4 py-3 border-b border-gray-100 dark:border-gray-700/50">
            <Radio.Group
              type="button"
              value={activeTab}
              onChange={(val) => setActiveTab(val as "history" | "favorites")}
              size="small"
            >
              <Radio value="history">今日历史</Radio>
              <Radio value="favorites">收藏夹</Radio>
            </Radio.Group>
            <Input
              ref={inputRef as any}
              size="small"
              placeholder="搜索..."
              prefix={<IconSearch style={{ color: "rgb(156 163 175)", fontSize: 16 }} />}
              suffix={
                <div className="flex items-center gap-1">
                  {searchQuery && (
                    <IconClose
                      style={{ color: "rgb(156 163 175)", fontSize: 14, cursor: "pointer" }}
                      onClick={() => setSearchQuery("")}
                    />
                  )}
                  <IconImage
                    style={{ fontSize: 15, cursor: "pointer", color: showImageOnly ? "rgb(99 102 241)" : "rgb(156 163 175)" }}
                    onClick={toggleShowImageOnly}
                  />
                </div>
              }
              value={searchQuery}
              onChange={setSearchQuery}
              style={{ flex: 1, background: "rgb(249 250 251)", border: "none", borderRadius: 8 }}
            />
          </div>

          {activeTab === "history" ? <HistoryList /> : <FavoriteList />}
        </div>
      )}
    </div>
  );
}

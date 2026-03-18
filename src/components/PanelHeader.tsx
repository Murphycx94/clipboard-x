import { Button } from "@arco-design/web-react";
import { IconArchive, IconSettings, IconClose } from "@arco-design/web-react/icon";
import { getCurrentWindow, PhysicalPosition } from "@tauri-apps/api/window";

type View = "main" | "archive" | "settings";

interface Props {
  view: View;
  onViewChange: (view: View) => void;
}

const hideWindow = () => getCurrentWindow().setPosition(new PhysicalPosition(-10000, -10000));

export function PanelHeader({ view, onViewChange }: Props) {
  return (
    <div
      data-tauri-drag-region
      className="flex items-center justify-between px-4 py-2 border-b border-gray-100 select-none"
      style={{ minHeight: 44 }}
    >
      <span
        className="font-semibold text-gray-800 text-sm cursor-pointer"
        onClick={() => onViewChange("main")}
      >ClipboardX</span>
      <div className="flex items-center gap-0.5">
        <Button
          type="text"
          size="mini"
          icon={<IconArchive style={{ fontSize: 16 }} />}
          onClick={() => onViewChange(view === "archive" ? "main" : "archive")}
          style={{ color: view === "archive" ? "rgb(99 102 241)" : "rgb(156 163 175)" }}
        />
        <Button
          type="text"
          size="mini"
          icon={<IconSettings style={{ fontSize: 16 }} />}
          onClick={() => onViewChange(view === "settings" ? "main" : "settings")}
          style={{ color: view === "settings" ? "rgb(99 102 241)" : "rgb(156 163 175)" }}
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
  );
}

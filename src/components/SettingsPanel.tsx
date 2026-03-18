import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { getSettings, updateHotkey } from "../api/commands";

export function SettingsPanel() {
  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: getSettings,
  });

  const [hotkey, setHotkey] = useState("");

  const mutation = useMutation({
    mutationFn: updateHotkey,
  });

  const currentHotkey = settings?.hotkey ?? "CmdOrCtrl+Shift+V";

  return (
    <div className="flex-1 p-4 space-y-4 overflow-y-auto">
      <h2 className="text-sm font-semibold text-gray-700">设置</h2>

      <div className="space-y-2">
        <label className="text-xs font-medium text-gray-500">全局唤出快捷键</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={hotkey || currentHotkey}
            onChange={(e) => setHotkey(e.target.value)}
            placeholder={currentHotkey}
            className="flex-1 px-3 py-1.5 text-sm border border-gray-200 rounded-lg outline-none focus:border-indigo-400"
          />
          <button
            onClick={() => mutation.mutate(hotkey || currentHotkey)}
            disabled={mutation.isPending}
            className="px-3 py-1.5 text-sm bg-indigo-500 text-white rounded-lg hover:bg-indigo-600 disabled:opacity-50"
          >
            保存
          </button>
        </div>
        <p className="text-xs text-gray-400">
          例如: CmdOrCtrl+Shift+V、Alt+Space
        </p>
        {mutation.isSuccess && (
          <p className="text-xs text-green-500">快捷键已更新，重启后生效</p>
        )}
      </div>
    </div>
  );
}

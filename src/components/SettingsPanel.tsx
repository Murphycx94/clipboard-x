import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { Input, Button, Typography } from "@arco-design/web-react";
import { getSettings, updateHotkey } from "../api/commands";

export function SettingsPanel() {
  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: getSettings,
  });

  const [hotkey, setHotkey] = useState("");
  const mutation = useMutation({ mutationFn: updateHotkey });
  const currentHotkey = settings?.hotkey ?? "CmdOrCtrl+Shift+V";

  return (
    <div className="flex-1 p-4 overflow-y-auto">
      <Typography.Title heading={6} style={{ marginBottom: 16 }}>设置</Typography.Title>

      <div className="space-y-2">
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>全局唤出快捷键</Typography.Text>
        <div className="flex gap-2">
          <Input
            size="small"
            value={hotkey || currentHotkey}
            onChange={setHotkey}
            placeholder={currentHotkey}
          />
          <Button
            type="primary"
            size="small"
            loading={mutation.isPending}
            onClick={() => mutation.mutate(hotkey || currentHotkey)}
          >
            保存
          </Button>
        </div>
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          例如: CmdOrCtrl+Shift+V、Alt+Space
        </Typography.Text>
        {mutation.isSuccess && (
          <Typography.Text style={{ fontSize: 12, color: "rgb(34 197 94)" }}>
            快捷键已更新，重启后生效
          </Typography.Text>
        )}
      </div>
    </div>
  );
}

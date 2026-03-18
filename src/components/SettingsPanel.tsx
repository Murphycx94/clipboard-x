import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { Input, Button, Switch, List, Message } from "@arco-design/web-react";
import { IconSettings, IconCommand } from "@arco-design/web-react/icon";
import { getSettings, updateHotkey } from "../api/commands";

const HIDE_ON_BLUR_KEY = "clipboardx_hide_on_blur";

export function getHideOnBlur(): boolean {
  return localStorage.getItem(HIDE_ON_BLUR_KEY) !== "false";
}

export function SettingsPanel() {
  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: getSettings,
  });

  const [hotkey, setHotkey] = useState("");
  const [currentHotkey, setCurrentHotkey] = useState(
    settings?.hotkey ?? "CmdOrCtrl+Shift+V",
  );
  const [hideOnBlur, setHideOnBlur] = useState(getHideOnBlur);
  const mutation = useMutation({
    mutationFn: updateHotkey,
    onSuccess: () => {
      Message.normal("快捷键已更新，重启后生效");
      setCurrentHotkey(hotkey);
    },
  });

  const handleHideOnBlurChange = (val: boolean) => {
    setHideOnBlur(val);
    localStorage.setItem(HIDE_ON_BLUR_KEY, String(val));
  };

  return (
    <div className="flex-1 overflow-y-auto">
      <List bordered={false} split>
        <List.Item
          extra={
            <Switch
              checked={hideOnBlur}
              onChange={handleHideOnBlurChange}
              size="small"
            />
          }
        >
          <List.Item.Meta
            avatar={
              <IconSettings
                style={{ fontSize: 16, color: "rgb(107 114 128)" }}
              />
            }
            title="失焦自动隐藏"
            description="切换到其他应用时自动收起窗口"
          />
        </List.Item>

        <List.Item
          extra={
            <div className="flex gap-2 items-center justify-between">
              <Input
                size="small"
                style={{ width: 160 }}
                value={hotkey || currentHotkey}
                onChange={setHotkey}
                placeholder={currentHotkey}
              />
              <Button
                type="primary"
                size="small"
                loading={mutation.isPending}
                onClick={() => {
                  if (!hotkey || hotkey === currentHotkey) return;
                  mutation.mutate(hotkey);
                }}
              >
                保存
              </Button>
            </div>
          }
        >
          <List.Item.Meta
            avatar={
              <IconCommand
                style={{ fontSize: 16, color: "rgb(107 114 128)" }}
              />
            }
            title="全局唤出快捷键"
            description="例如：CmdOrCtrl+Shift+V"
          />
        </List.Item>
      </List>
    </div>
  );
}

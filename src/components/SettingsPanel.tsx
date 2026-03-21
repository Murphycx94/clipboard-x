import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Input,
  Button,
  Switch,
  List,
  Message,
  Select,
  Modal,
  Radio,
} from "@arco-design/web-react";
import {
  IconSettings,
  IconCommand,
  IconClockCircle,
  IconDelete,
} from "@arco-design/web-react/icon";
import {
  getSettings,
  updateHotkey,
  updateRetention,
  clearHistory,
} from "../api/commands";
import { type Theme, getTheme, setTheme } from "../hooks/useTheme";
import { isMac } from "../utils/platform";

const HIDE_ON_BLUR_KEY = "clipboardx_hide_on_blur";

export function getHideOnBlur(): boolean {
  return localStorage.getItem(HIDE_ON_BLUR_KEY) !== "false";
}

const RETENTION_OPTIONS = [
  { label: "一周", value: 7 },
  { label: "一个月", value: 30 },
  { label: "一年", value: 365 },
  { label: "永久保留", value: 0 },
];

export function SettingsPanel() {
  const queryClient = useQueryClient();
  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: getSettings,
  });

  const [hotkey, setHotkey] = useState("");
  const [currentHotkey, setCurrentHotkey] = useState(
    settings?.hotkey ?? "CmdOrCtrl+Shift+V",
  );
  const [hideOnBlur, setHideOnBlur] = useState(getHideOnBlur);
  const [currentTheme, setCurrentTheme] = useState<Theme>(getTheme);

  const hotKeyMutation = useMutation({
    mutationFn: updateHotkey,
    onSuccess: () => {
      Message.success("快捷键已更新，重启后生效");
      setCurrentHotkey(hotkey);
    },
  });

  const retentionMutation = useMutation({
    mutationFn: updateRetention,
    onSuccess: () => Message.success("保留时间已更新"),
  });

  const clearMutation = useMutation({
    mutationFn: clearHistory,
    onSuccess: (count) => {
      queryClient.invalidateQueries();
      Message.success(`已清除 ${count} 条历史记录`);
    },
  });

  const handleHideOnBlurChange = (val: boolean) => {
    setHideOnBlur(val);
    localStorage.setItem(HIDE_ON_BLUR_KEY, String(val));
  };

  const handleThemeChange = (val: Theme) => {
    setCurrentTheme(val);
    setTheme(val);
  };

  const handleClearHistory = () => {
    Modal.confirm({
      title: "清除历史记录",
      content: "将删除所有未收藏的历史记录，此操作不可撤销。",
      okText: "确认清除",
      cancelText: "取消",
      okButtonProps: { status: "danger" },
      onOk: () => clearMutation.mutate(),
    });
  };

  const currentRetention = settings?.retention_days ?? 7;

  return (
    <div className="flex-1 overflow-y-auto">
      <div className="px-4 pt-2">
        <span className="font-medium text-gray-400 uppercase tracking-wider">
          设置
        </span>
      </div>
      <List bordered={false} split>
        <List.Item
          extra={
            <div className="flex gap-2 items-center">
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
                loading={hotKeyMutation.isPending}
                onClick={() => {
                  if (!hotkey || hotkey === currentHotkey) return;
                  hotKeyMutation.mutate(hotkey);
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
            description={isMac ? "例如：CmdOrCtrl+Shift+V（Mac 显示为 ⌘⇧V）" : "例如：CmdOrCtrl+Shift+V（Windows 显示为 Ctrl+Shift+V）"}
          />
        </List.Item>

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
            <Radio.Group
              type="button"
              size="small"
              value={currentTheme}
              onChange={handleThemeChange}
            >
              <Radio value="light">浅色</Radio>
              <Radio value="dark">深色</Radio>
              <Radio value="system">跟随系统</Radio>
            </Radio.Group>
          }
        >
          <List.Item.Meta
            avatar={
              <IconSettings
                style={{ fontSize: 16, color: "rgb(107 114 128)" }}
              />
            }
            title="外观主题"
            description="选择界面显示模式"
          />
        </List.Item>

        <List.Item
          extra={
            <Select
              size="small"
              style={{ width: 100 }}
              defaultValue={currentRetention}
              options={RETENTION_OPTIONS}
              onChange={(val) => retentionMutation.mutate(val)}
            />
          }
        >
          <List.Item.Meta
            avatar={
              <IconClockCircle
                style={{ fontSize: 16, color: "rgb(107 114 128)" }}
              />
            }
            title="历史保留时间"
            description="非收藏内容的自动清理周期"
          />
        </List.Item>

        <List.Item
          extra={
            <Button
              size="small"
              status="danger"
              icon={<IconDelete />}
              loading={clearMutation.isPending}
              onClick={handleClearHistory}
            >
              立即清除
            </Button>
          }
        >
          <List.Item.Meta
            avatar={
              <IconDelete style={{ fontSize: 16, color: "rgb(107 114 128)" }} />
            }
            title="清除历史缓存"
            description="清除所有未收藏的历史记录"
          />
        </List.Item>
      </List>
    </div>
  );
}

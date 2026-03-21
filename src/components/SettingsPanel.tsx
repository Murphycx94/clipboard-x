import { useState, useEffect } from "react";
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
  IconMessage,
  IconFile,
  IconRefresh,
} from "@arco-design/web-react/icon";
import {
  getSettings,
  updateHotkey,
  updateRetention,
  clearHistory,
  updateTelegramToken,
  updateTelegramChatId,
  getLogs,
} from "../api/commands";
import { type Theme, getTheme, setTheme } from "../hooks/useTheme";
import { isMac } from "../utils/platform";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";

const HIDE_ON_BLUR_KEY = "clipboardx_hide_on_blur";

export function getHideOnBlur(): boolean {
  return localStorage.getItem(HIDE_ON_BLUR_KEY) !== "false";
}

function maskToken(token: string): string {
  if (!token) return "";
  if (token.length <= 8) return "****";
  return token.slice(0, 4) + "****" + token.slice(-4);
}

const RETENTION_OPTIONS = [
  { label: "一周", value: 7 },
  { label: "一个月", value: 30 },
  { label: "一年", value: 365 },
  { label: "永久保留", value: 0 },
];

function TelegramModal({
  visible,
  onClose,
  initialTokenMasked,
  initialChatId,
}: {
  visible: boolean;
  onClose: () => void;
  initialTokenMasked: string;
  initialChatId: string;
}) {
  const [tokenDisplay, setTokenDisplay] = useState(initialTokenMasked);
  const [tokenFocused, setTokenFocused] = useState(false);
  const [tokenDraft, setTokenDraft] = useState("");
  const [chatId, setChatId] = useState(initialChatId);
  const [chatIdSaved, setChatIdSaved] = useState(initialChatId);
  const [logVisible, setLogVisible] = useState(false);
  const [logContent, setLogContent] = useState("");

  useEffect(() => {
    setTokenDisplay(initialTokenMasked);
    setChatId(initialChatId);
    setChatIdSaved(initialChatId);
  }, [initialTokenMasked, initialChatId]);

  const handleTokenFocus = () => {
    setTokenFocused(true);
    setTokenDraft("");
  };

  const handleTokenBlur = async () => {
    setTokenFocused(false);
    if (tokenDraft && tokenDraft.length > 8) {
      await updateTelegramToken(tokenDraft);
      setTokenDisplay(maskToken(tokenDraft));
      Message.success("Token 已保存");
    }
    setTokenDraft("");
  };

  const handleChatIdSave = async () => {
    if (!chatId || chatId === chatIdSaved) return;
    await updateTelegramChatId(chatId);
    setChatIdSaved(chatId);
    Message.success("Chat ID 已保存");
  };

  const handleViewLogs = async () => {
    const content = await getLogs();
    setLogContent(content);
    setLogVisible(true);
  };

  const isConfigured = !!(tokenDisplay && chatIdSaved);

  return (
    <>
      <Modal
        title="配置 Telegram Bot"
        visible={visible}
        onCancel={onClose}
        footer={null}
        style={{ width: 360 }}
        autoFocus={false}
      >
        <div className="flex flex-col gap-4 py-1">
          {/* Status */}
          <div className="flex items-center gap-2 text-xs text-gray-400 dark:text-gray-500">
            <span
              className={`inline-block w-1.5 h-1.5 rounded-full ${isConfigured ? "bg-green-400" : "bg-gray-300 dark:bg-gray-600"}`}
            />
            {isConfigured ? "已配置，新增收藏时自动同步" : "未配置，填写下方信息后生效"}
          </div>

          {/* Token */}
          <div className="flex flex-col gap-1">
            <span className="text-xs text-gray-500 dark:text-gray-400">Bot Token</span>
            <Input
              value={tokenFocused ? tokenDraft : tokenDisplay}
              onChange={setTokenDraft}
              onFocus={handleTokenFocus}
              onBlur={handleTokenBlur}
              placeholder="输入 Bot Token，失焦后自动保存"
              type={tokenFocused ? "text" : "password"}
            />
          </div>

          {/* Chat ID */}
          <div className="flex flex-col gap-1">
            <span className="text-xs text-gray-500 dark:text-gray-400">
              Chat ID
              <a
                className="ml-1 text-indigo-400 hover:text-indigo-500"
                href="https://t.me/userinfobot"
                target="_blank"
                rel="noreferrer"
              >
                （@userinfobot 获取）
              </a>
            </span>
            <div className="flex gap-2">
              <Input
                value={chatId}
                onChange={setChatId}
                placeholder="输入 Chat ID"
                className="flex-1"
              />
              <Button type="primary" onClick={handleChatIdSave}>
                保存
              </Button>
            </div>
          </div>

          {/* Logs */}
          <div className="flex justify-end pt-1 border-t border-gray-100 dark:border-gray-700">
            <Button
              type="text"
              size="small"
              icon={<IconFile style={{ fontSize: 13 }} />}
              onClick={handleViewLogs}
              style={{ color: "rgb(156 163 175)" }}
            >
              查看同步日志
            </Button>
          </div>
        </div>
      </Modal>

      <Modal
        title="同步日志"
        visible={logVisible}
        onCancel={() => setLogVisible(false)}
        footer={null}
        style={{ width: 360 }}
      >
        <pre
          className="overflow-auto p-3 text-xs text-gray-700 bg-gray-50 rounded dark:text-gray-300 dark:bg-gray-800"
          style={{ maxHeight: 400, whiteSpace: "pre-wrap", wordBreak: "break-all" }}
        >
          {logContent || "暂无日志"}
        </pre>
      </Modal>
    </>
  );
}

export function SettingsPanel() {
  const queryClient = useQueryClient();
  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: getSettings,
  });

  const [hotkey, setHotkey] = useState("");
  const [currentHotkey, setCurrentHotkey] = useState(
    settings?.hotkey ?? (isMac ? "Cmd+Shift+V" : "Ctrl+Shift+V"),
  );
  const [hideOnBlur, setHideOnBlur] = useState(getHideOnBlur);
  const [currentTheme, setCurrentTheme] = useState<Theme>(getTheme);
  const [telegramModalVisible, setTelegramModalVisible] = useState(false);

  const [appVersion, setAppVersion] = useState("");
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateModal, setUpdateModal] = useState<{ visible: boolean; version: string; body: string; install: () => Promise<void> }>({
    visible: false, version: "", body: "", install: async () => {},
  });

  useEffect(() => {
    getVersion().then(setAppVersion);
  }, []);

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    try {
      const update = await check();
      if (update) {
        setUpdateModal({
          visible: true,
          version: update.version,
          body: update.body ?? "",
          install: async () => {
            await update.downloadAndInstall();
            await relaunch();
          },
        });
      } else {
        Message.success("当前已是最新版本");
      }
    } catch {
      Message.error("检查更新失败，请稍后重试");
    } finally {
      setCheckingUpdate(false);
    }
  };

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
  const isConfigured = !!(settings?.telegram_token_masked && settings?.telegram_chat_id);

  return (
    <div className="overflow-y-auto flex-1">
      <div className="px-4 pt-2">
        <span className="font-medium tracking-wider text-gray-400 uppercase">
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
            avatar={<IconCommand style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="全局唤出快捷键"
            description={isMac ? "例如：Cmd+Shift+V" : "例如：Ctrl+Shift+V"}
          />
        </List.Item>

        <List.Item
          extra={
            <Switch checked={hideOnBlur} onChange={handleHideOnBlurChange} size="small" />
          }
        >
          <List.Item.Meta
            avatar={<IconSettings style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="失焦自动隐藏"
            description="切换到其他应用时自动收起窗口"
          />
        </List.Item>

        <List.Item
          extra={
            <Radio.Group type="button" size="small" value={currentTheme} onChange={handleThemeChange}>
              <Radio value="light">浅色</Radio>
              <Radio value="dark">深色</Radio>
              <Radio value="system">跟随系统</Radio>
            </Radio.Group>
          }
        >
          <List.Item.Meta
            avatar={<IconSettings style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
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
            avatar={<IconClockCircle style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="历史保留时间"
            description="非收藏内容的自动清理周期"
          />
        </List.Item>

        <List.Item
          extra={
            <Button
              size="small"
              loading={checkingUpdate}
              icon={<IconRefresh />}
              onClick={handleCheckUpdate}
            >
              检查更新
            </Button>
          }
        >
          <List.Item.Meta
            avatar={<IconSettings style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="当前版本"
            description={appVersion ? `v${appVersion}` : "加载中…"}
          />
        </List.Item>

        <List.Item
          extra={
            <Button size="small" onClick={() => setTelegramModalVisible(true)}>
              {isConfigured ? "已配置" : "设置"}
            </Button>
          }
        >
          <List.Item.Meta
            avatar={<IconMessage style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="配置 Telegram Bot"
            description="用于同步收藏夹内容"
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
            avatar={<IconDelete style={{ fontSize: 16, color: "rgb(107 114 128)" }} />}
            title="清除历史缓存"
            description="清除所有未收藏的历史记录"
          />
        </List.Item>
      </List>

      <TelegramModal
        visible={telegramModalVisible}
        onClose={() => setTelegramModalVisible(false)}
        initialTokenMasked={settings?.telegram_token_masked ?? ""}
        initialChatId={settings?.telegram_chat_id ?? ""}
      />

      <Modal
        title={`发现新版本 v${updateModal.version}`}
        visible={updateModal.visible}
        onCancel={() => setUpdateModal((s) => ({ ...s, visible: false }))}
        style={{ width: 360 }}
        autoFocus={false}
        footer={
          <div className="flex justify-end gap-2">
            <Button onClick={() => setUpdateModal((s) => ({ ...s, visible: false }))}>
              稍后再说
            </Button>
            <Button
              type="primary"
              onClick={async () => {
                setUpdateModal((s) => ({ ...s, visible: false }));
                const hide = Message.loading({ content: "正在下载更新…", duration: 0 });
                try {
                  await updateModal.install();
                } catch {
                  hide();
                  Message.error("更新失败，请稍后重试");
                }
              }}
            >
              立即升级
            </Button>
          </div>
        }
      >
        {updateModal.body ? (
          <pre className="text-xs text-gray-600 dark:text-gray-300 whitespace-pre-wrap">
            {updateModal.body}
          </pre>
        ) : (
          <p className="text-sm text-gray-500">有新版本可用，是否立即升级？</p>
        )}
      </Modal>
    </div>
  );
}

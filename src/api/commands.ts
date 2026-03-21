import { invoke } from "@tauri-apps/api/core";
import { ClipboardItem, DateGroup, AppSettings } from "../types";

export const getTodayItems = (): Promise<ClipboardItem[]> =>
  invoke("get_today_items");

export const getArchiveItems = (): Promise<DateGroup[]> =>
  invoke("get_archive_items");

export const getFavoriteItems = (): Promise<ClipboardItem[]> =>
  invoke("get_favorite_items");

export const toggleFavorite = (id: number, note: string): Promise<void> =>
  invoke("toggle_favorite", { id, note });

export const copyToClipboard = (id: number): Promise<void> =>
  invoke("copy_to_clipboard", { id });

export const deleteItem = (id: number): Promise<void> =>
  invoke("delete_item", { id });

export const getSettings = (): Promise<AppSettings> =>
  invoke("get_settings");

export const updateHotkey = (hotkey: string): Promise<void> =>
  invoke("update_hotkey", { hotkey });

export const updateNote = (id: number, note: string): Promise<void> =>
  invoke("update_note", { id, note });

export const getImageBase64 = (id: number): Promise<string | null> =>
  invoke("get_image_base64", { id });

export const updateRetention = (days: number): Promise<void> =>
  invoke("update_retention", { days });

export const clearHistory = (): Promise<number> =>
  invoke("clear_history");

export const hideWindow = (): Promise<void> =>
  invoke("hide_window");

export const updateTelegramToken = (token: string): Promise<void> =>
  invoke("update_telegram_token", { token });

export const updateTelegramChatId = (chatId: string): Promise<void> =>
  invoke("update_telegram_chat_id", { chatId });

export const getLogs = (): Promise<string> =>
  invoke("get_logs");

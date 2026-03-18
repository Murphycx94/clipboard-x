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

export type ContentType = "text" | "image";

export interface ClipboardItem {
  id: number;
  content_type: ContentType;
  text_content: string | null;
  image_thumbnail: string | null; // base64
  created_at: string; // ISO datetime
  is_favorite: boolean;
  note: string | null;
}

export interface DateGroup {
  date: string; // YYYY-MM-DD
  items: ClipboardItem[];
}

export interface AppSettings {
  hotkey: string;
  retention_days: number; // 0 = 永久保留
}

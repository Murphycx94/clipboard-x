import { create } from "zustand";

type Tab = "history" | "favorites";

interface ClipboardStore {
  activeTab: Tab;
  searchQuery: string;
  setActiveTab: (tab: Tab) => void;
  setSearchQuery: (q: string) => void;
}

export const useClipboardStore = create<ClipboardStore>((set) => ({
  activeTab: "history",
  searchQuery: "",
  setActiveTab: (tab) => set({ activeTab: tab }),
  setSearchQuery: (q) => set({ searchQuery: q }),
}));

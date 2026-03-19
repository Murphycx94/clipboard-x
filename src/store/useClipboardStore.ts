import { create } from "zustand";

type Tab = "history" | "favorites";

interface ClipboardStore {
  activeTab: Tab;
  searchQuery: string;
  focusedIndex: number;
  showImageOnly: boolean;
  setActiveTab: (tab: Tab) => void;
  setSearchQuery: (q: string) => void;
  setFocusedIndex: (index: number) => void;
  toggleShowImageOnly: () => void;
}

export const useClipboardStore = create<ClipboardStore>((set) => ({
  activeTab: "history",
  searchQuery: "",
  focusedIndex: 0,
  showImageOnly: false,
  setActiveTab: (tab) => set({ activeTab: tab }),
  setSearchQuery: (q) => set({ searchQuery: q }),
  setFocusedIndex: (index) => set({ focusedIndex: index }),
  toggleShowImageOnly: () => set((s) => ({ showImageOnly: !s.showImageOnly })),
}));

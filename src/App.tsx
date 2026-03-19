import { useEffect } from "react";
import { initTheme } from "./hooks/useTheme";
import { MainPanel } from "./components/MainPanel";

export default function App() {
  useEffect(() => { initTheme(); }, []);
  return <MainPanel />;
}

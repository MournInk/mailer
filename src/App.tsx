/**
 * App shell: three-pane mail layout (sidebar / list / reading pane),
 * settings view, onboarding, plus global overlays (alerts, compose, toasts).
 */

import { useApp } from "./lib/store";
import { Sidebar } from "./components/Sidebar";
import { MessageList } from "./components/MessageList";
import { MessageView } from "./components/MessageView";
import { SettingsView } from "./components/settings/SettingsView";
import { Onboarding } from "./components/Onboarding";
import { AlertCenter } from "./components/AlertCenter";
import { Compose } from "./components/Compose";
import { TitleBar } from "./components/TitleBar";
import "./App.css";

export default function App() {
  const { view, accounts, compose } = useApp();

  return (
    <div className="app-shell">
      <TitleBar />
      {view === "settings" ? (
        <SettingsView />
      ) : accounts.length === 0 ? (
        <Onboarding />
      ) : (
        <div className="mail-layout">
          <Sidebar />
          <MessageList />
          <MessageView />
        </div>
      )}
      <AlertCenter />
      {compose && <Compose />}
    </div>
  );
}

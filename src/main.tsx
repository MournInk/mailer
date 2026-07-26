import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { AppProvider } from "./lib/store";
import "./styles/tokens.css";
import "./styles/base.css";

// The boundary wraps the provider, not just the app: a throw while the provider
// sets up its Tauri event listeners would otherwise unmount everything and
// leave an empty window.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <AppProvider>
        <App />
      </AppProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);

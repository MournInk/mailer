/**
 * Last line of defence against a blank window.
 *
 * A throw during render or inside an effect unmounts the whole React tree,
 * leaving the user staring at an empty frame with no way to tell whether the
 * app is broken, still loading, or waiting on something. Showing the error —
 * and a way out — is always better than showing nothing.
 */

import { Component, type ErrorInfo, type ReactNode } from "react";
import "./ErrorBoundary.css";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
  stack: string;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, stack: "" };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Reaches the terminal on desktop and the device log on mobile.
    console.error("Mailer crashed:", error, info.componentStack);
    this.setState({ stack: info.componentStack ?? "" });
  }

  render() {
    const { error, stack } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="crash">
        <div className="crash-card">
          <h1 className="crash-title">应用遇到了问题</h1>
          <p className="crash-lede">
            界面加载失败。下面是具体错误，重试通常可以恢复；如果每次启动都这样，请把这段信息反馈给我们。
          </p>
          <pre className="crash-detail">
            {error.message}
            {stack ? `\n${stack.trim()}` : ""}
          </pre>
          <div className="crash-actions">
            <button className="btn btn-primary" onClick={() => window.location.reload()}>
              重新加载
            </button>
            <button
              className="btn"
              onClick={() => {
                void navigator.clipboard?.writeText(`${error.message}\n${stack}`);
              }}
            >
              复制错误信息
            </button>
          </div>
        </div>
      </div>
    );
  }
}

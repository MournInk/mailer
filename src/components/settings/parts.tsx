/**
 * Small building blocks shared by the four settings tabs: the section card,
 * the titled group inside one, the label + switch row, and the block that
 * renders a `TestResult`.
 */

import type { ReactNode } from "react";
import { Icon } from "../Icon";
import type { TestResult } from "../../lib/types";

/**
 * A titled card. `icon` renders the quiet chip on the title row (same
 * treatment as the onboarding feature rows); `action` sits right-aligned.
 */
export function Section({
  title,
  sub,
  icon,
  action,
  children,
}: {
  title: string;
  sub?: string;
  icon?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="card set-section">
      <header className="set-section-head">
        {icon && (
          <span className="set-section-mark">
            <Icon name={icon} size={15} />
          </span>
        )}
        <div className="set-section-text">
          <h2 className="set-section-title">{title}</h2>
          {sub && <p className="set-section-sub">{sub}</p>}
        </div>
        {action}
      </header>
      <div className="set-section-body">{children}</div>
    </section>
  );
}

/**
 * A titled group of fields inside a section body. The rule above it runs the
 * full width of the card, so a long form reads as a few labelled blocks
 * instead of one undifferentiated stack of inputs.
 */
export function Group({
  title,
  hint,
  action,
  children,
}: {
  title: string;
  hint?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="set-group">
      <div className="set-group-head">
        <div className="set-group-text">
          <h3 className="set-group-title">{title}</h3>
          {hint && <p className="set-group-hint">{hint}</p>}
        </div>
        {action}
      </div>
      {children}
    </div>
  );
}

/**
 * Label + description on the left, switch on the right. The whole row is the
 * hit target — a 38px switch is a poor one on a phone.
 */
export function SwitchField({
  label,
  hint,
  checked,
  onChange,
  disabled = false,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className={`set-switch-row${checked ? " on" : ""}`}
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="set-switch-text">
        <span className="set-switch-label">{label}</span>
        {hint && <span className="field-hint">{hint}</span>}
      </span>
      <span className={`switch${checked ? " on" : ""}`} />
    </button>
  );
}

/**
 * Connection-test output. The status line says only which way it went; the
 * backend's multi-line, already-localized text sits below it in a mono block
 * and is rendered verbatim, never reworded.
 */
export function TestOutput({ result }: { result: TestResult | null }) {
  if (!result) return null;
  return (
    <div className={`set-test${result.ok ? " ok" : " bad"}`} role="status">
      <div className="set-test-head">
        <span className="set-test-mark">
          <Icon name={result.ok ? "check" : "alert"} size={13} />
        </span>
        <span className="set-test-title">{result.ok ? "连接成功" : "连接失败"}</span>
      </div>
      <pre className="set-test-text">{result.message}</pre>
    </div>
  );
}
